//! Direct SQL ingestion of parsed legal documents into the vault.
//!
//! This is a dedicated path that **bypasses** [`VaultStore::ingest_markdown`]
//! (and its `WikilinkPipeline`) because:
//!   1. Legal citations are not fuzzy wikilinks — we have exact edges from
//!      regex-based extraction.
//!   2. AI-driven keyword gatekeeping is inappropriate: every statute article
//!      and precedent is knowledge by construction; hallucinated filtering
//!      would silently drop law.
//!   3. We need deterministic, auditable writes.
//!
//! Write layout per call:
//!   - Statute file → one `vault_documents` row **per article**, plus
//!     aliases, frontmatter, tags, and edges (intra-law + cross-law).
//!   - Case file → one `vault_documents` row for the case, plus aliases,
//!     frontmatter, tags, and edges (statute citations + case citations).
//!
//! Slugs live in `vault_documents.title` so the existing link-resolution
//! lookup (`SELECT id FROM vault_documents WHERE title = ?`) wires edges
//! whenever the target is already present. After ingest we also call
//! [`resolve_pending_links`] to pick up targets added earlier in the same
//! batch.

use super::case_extractor::CaseDoc;
use super::slug::statute_aliases;
use super::statute_extractor::{StatuteArticle, StatuteDoc};
use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Running totals reported back to the CLI.
#[derive(Debug, Default, Clone)]
pub struct IngestCounts {
    pub statute_files: usize,
    pub statute_articles_inserted: usize,
    pub statute_articles_skipped_unchanged: usize,
    pub statute_articles_updated: usize,
    pub case_files: usize,
    pub cases_inserted: usize,
    pub cases_skipped_unchanged: usize,
    pub cases_updated: usize,
    pub edges_written: usize,
    pub edges_resolved_after_pass: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct IngestReport {
    pub counts: IngestCounts,
}

// ───────── Statute ingestion ─────────

pub fn ingest_statute(conn: &Arc<Mutex<Connection>>, doc: &StatuteDoc) -> Result<IngestCounts> {
    let mut counts = IngestCounts {
        statute_files: 1,
        ..Default::default()
    };

    // Run the whole statute as one transaction — all-or-nothing.
    let mut guard = conn.lock();
    let tx = guard
        .transaction()
        .context("starting transaction for statute ingest")?;
    for article in &doc.articles {
        match upsert_statute_article(&tx, doc, article)? {
            ArticleOutcome::Inserted(doc_id) => {
                counts.statute_articles_inserted += 1;
                counts.edges_written += write_statute_edges(&tx, doc, article, doc_id)?;
            }
            ArticleOutcome::Updated(doc_id) => {
                counts.statute_articles_updated += 1;
                counts.edges_written += write_statute_edges(&tx, doc, article, doc_id)?;
            }
            ArticleOutcome::SkippedUnchanged => {
                counts.statute_articles_skipped_unchanged += 1;
            }
        }
    }
    tx.commit().context("committing statute ingest transaction")?;
    Ok(counts)
}

enum ArticleOutcome {
    Inserted(i64),
    Updated(i64),
    SkippedUnchanged,
}

fn upsert_statute_article(
    conn: &Connection,
    doc: &StatuteDoc,
    article: &StatuteArticle,
) -> Result<ArticleOutcome> {
    // Per-article content: include a brief header so the full article body
    // renders nicely in any plain-text view while keeping the JSON metadata
    // in `vault_frontmatter`.
    let content = format!(
        "# {} {}\n\n{}\n",
        doc.law_name, article.header, article.body
    );
    let checksum = hex_sha256(&content);
    let now = unix_epoch();

    // Check by canonical slug.
    let existing: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, checksum FROM vault_documents WHERE title = ?1",
            params![article.slug],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .ok();

    let doc_id: i64 = match existing {
        Some((_id, existing_sum)) if existing_sum == checksum => {
            return Ok(ArticleOutcome::SkippedUnchanged);
        }
        Some((id, _)) => {
            conn.execute(
                "UPDATE vault_documents
                   SET content = ?1, checksum = ?2, char_count = ?3, updated_at = ?4,
                       original_path = ?5, doc_type = 'statute_article', source_type = 'local_file'
                 WHERE id = ?6",
                params![
                    content,
                    checksum,
                    content.chars().count() as i64,
                    now as i64,
                    doc.source_path,
                    id,
                ],
            )
            .context("updating existing statute article row")?;
            // Replace links + auxiliary rows cleanly.
            conn.execute(
                "DELETE FROM vault_links WHERE source_doc_id = ?1",
                params![id],
            )?;
            conn.execute(
                "DELETE FROM vault_frontmatter WHERE doc_id = ?1",
                params![id],
            )?;
            conn.execute("DELETE FROM vault_tags WHERE doc_id = ?1", params![id])?;
            // Aliases are globally UNIQUE; leaving stale ones risks blocking
            // valid aliases on a renamed law — wipe them too.
            conn.execute("DELETE FROM vault_aliases WHERE doc_id = ?1", params![id])?;
            id
        }
        None => {
            let uuid = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO vault_documents
                    (uuid, title, content, source_type, source_device_id, original_path,
                     checksum, doc_type, char_count, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'local_file', ?4, ?5, ?6, 'statute_article', ?7, ?8, ?8)",
                params![
                    uuid,
                    article.slug,
                    content,
                    "local",
                    doc.source_path,
                    checksum,
                    content.chars().count() as i64,
                    now as i64,
                ],
            )
            .context("inserting statute article row")?;
            conn.last_insert_rowid()
        }
    };

    // Frontmatter (structured metadata).
    let frontmatter = [
        ("law_name", Some(doc.law_name.clone())),
        ("article_key", Some(article.article_key.clone())),
        ("article_num", Some(article.article_num.to_string())),
        (
            "article_sub",
            article.article_sub.map(|s| s.to_string()),
        ),
        ("article_title_kw", article.title_kw.clone()),
        ("article_header", Some(article.header.clone())),
        ("promulgated_at", doc.promulgated_at.clone()),
        ("effective_at", doc.effective_at.clone()),
        ("ls_id", doc.ls_id.clone()),
        ("ls_seq", doc.ls_seq.clone()),
        ("ancestry_no", doc.ancestry.clone()),
    ];
    for (k, v) in frontmatter {
        if let Some(val) = v {
            conn.execute(
                "INSERT OR IGNORE INTO vault_frontmatter (doc_id, key, value)
                 VALUES (?1, ?2, ?3)",
                params![doc_id, k, val],
            )?;
        }
    }

    // Aliases — human-friendly lookup forms. UNIQUE globally; skip on conflict.
    let aliases = statute_aliases(&doc.law_name, article.article_num, article.article_sub);
    for alias in aliases {
        let _ = conn.execute(
            "INSERT OR IGNORE INTO vault_aliases (doc_id, alias) VALUES (?1, ?2)",
            params![doc_id, alias],
        );
    }

    // Tags — domain/kind/law/keyword.
    insert_tag(conn, doc_id, "domain:legal", Some("domain"))?;
    insert_tag(conn, doc_id, "kind:statute", Some("kind"))?;
    insert_tag(conn, doc_id, &format!("law:{}", doc.law_name), Some("law"))?;
    if let Some(kw) = article.title_kw.as_deref() {
        insert_tag(conn, doc_id, kw, Some("title_kw"))?;
    }

    match existing {
        Some(_) => Ok(ArticleOutcome::Updated(doc_id)),
        None => Ok(ArticleOutcome::Inserted(doc_id)),
    }
}

fn write_statute_edges(
    conn: &Connection,
    doc: &StatuteDoc,
    article: &StatuteArticle,
    source_doc_id: i64,
) -> Result<usize> {
    let mut written = 0usize;
    for cite in &article.citations {
        let target_slug = super::slug::statute_slug(&cite.law_name, cite.article, cite.article_sub);
        // Don't self-edge.
        if target_slug == article.slug {
            continue;
        }
        let relation = if cite.law_name == doc.law_name {
            "internal-ref"
        } else {
            "cross-law"
        };
        insert_edge(
            conn,
            source_doc_id,
            &target_slug,
            relation,
            Some(&cite.raw),
        )?;
        written += 1;
    }
    Ok(written)
}

// ───────── Case ingestion ─────────

pub fn ingest_case(conn: &Arc<Mutex<Connection>>, doc: &CaseDoc) -> Result<IngestCounts> {
    let mut counts = IngestCounts {
        case_files: 1,
        ..Default::default()
    };
    let mut guard = conn.lock();
    let tx = guard
        .transaction()
        .context("starting transaction for case ingest")?;

    let checksum = hex_sha256(&doc.original_markdown);
    let now = unix_epoch();
    let existing: Option<(i64, String)> = tx
        .query_row(
            "SELECT id, checksum FROM vault_documents WHERE title = ?1",
            params![doc.slug],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .ok();

    let doc_id: i64 = match existing {
        Some((_id, existing_sum)) if existing_sum == checksum => {
            counts.cases_skipped_unchanged += 1;
            tx.commit()?;
            return Ok(counts);
        }
        Some((id, _)) => {
            tx.execute(
                "UPDATE vault_documents
                   SET content = ?1, checksum = ?2, char_count = ?3, updated_at = ?4,
                       original_path = ?5, doc_type = 'case', source_type = 'local_file'
                 WHERE id = ?6",
                params![
                    doc.original_markdown,
                    checksum,
                    doc.original_markdown.chars().count() as i64,
                    now as i64,
                    doc.source_path,
                    id,
                ],
            )?;
            tx.execute(
                "DELETE FROM vault_links WHERE source_doc_id = ?1",
                params![id],
            )?;
            tx.execute(
                "DELETE FROM vault_frontmatter WHERE doc_id = ?1",
                params![id],
            )?;
            tx.execute("DELETE FROM vault_tags WHERE doc_id = ?1", params![id])?;
            tx.execute("DELETE FROM vault_aliases WHERE doc_id = ?1", params![id])?;
            counts.cases_updated += 1;
            id
        }
        None => {
            let uuid = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO vault_documents
                    (uuid, title, content, source_type, source_device_id, original_path,
                     checksum, doc_type, char_count, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'local_file', ?4, ?5, ?6, 'case', ?7, ?8, ?8)",
                params![
                    uuid,
                    doc.slug,
                    doc.original_markdown,
                    "local",
                    doc.source_path,
                    checksum,
                    doc.original_markdown.chars().count() as i64,
                    now as i64,
                ],
            )?;
            counts.cases_inserted += 1;
            tx.last_insert_rowid()
        }
    };

    // Frontmatter.
    let frontmatter = [
        ("case_number", Some(doc.case_number.clone())),
        ("case_name", doc.case_name.clone()),
        ("court_name", doc.court_name.clone()),
        ("court_type_code", doc.court_type_code.clone()),
        ("case_category_code", doc.case_category_code.clone()),
        ("case_type_name", doc.case_type_name.clone()),
        ("precedent_serial_no", doc.precedent_serial_no.clone()),
        ("verdict_date", doc.verdict_date.clone()),
        ("verdict_kind", doc.verdict_kind.clone()),
        ("verdict_type", doc.verdict_type.clone()),
    ];
    for (k, v) in frontmatter {
        if let Some(val) = v {
            tx.execute(
                "INSERT OR IGNORE INTO vault_frontmatter (doc_id, key, value)
                 VALUES (?1, ?2, ?3)",
                params![doc_id, k, val],
            )?;
        }
    }

    // Aliases — full case number + court-qualified form.
    let _ = tx.execute(
        "INSERT OR IGNORE INTO vault_aliases (doc_id, alias) VALUES (?1, ?2)",
        params![doc_id, doc.case_number],
    );
    if let Some(court) = doc.court_name.as_deref() {
        let _ = tx.execute(
            "INSERT OR IGNORE INTO vault_aliases (doc_id, alias) VALUES (?1, ?2)",
            params![doc_id, format!("{court} {}", doc.case_number)],
        );
    }

    // Tags.
    insert_tag(&tx, doc_id, "domain:legal", Some("domain"))?;
    insert_tag(&tx, doc_id, "kind:case", Some("kind"))?;
    if let Some(c) = doc.court_name.as_deref() {
        insert_tag(&tx, doc_id, &format!("court:{c}"), Some("court"))?;
    }
    if let Some(c) = doc.case_type_name.as_deref() {
        insert_tag(&tx, doc_id, &format!("type:{c}"), Some("case_type"))?;
    }
    if let Some(d) = doc.verdict_date.as_deref() {
        if d.len() >= 4 {
            insert_tag(&tx, doc_id, &format!("year:{}", &d[..4]), Some("year"))?;
        }
    }

    // Edges: case → statute articles.
    let mut edges = 0usize;
    for cite in &doc.statute_citations {
        let target = super::slug::statute_slug(&cite.law_name, cite.article, cite.article_sub);
        insert_edge(&tx, doc_id, &target, "cites", Some(&cite.raw))?;
        edges += 1;
    }
    // Edges: case → case.
    for cref in &doc.case_citations {
        let target = super::slug::case_slug(&cref.case_number);
        if target == doc.slug {
            continue;
        }
        insert_edge(&tx, doc_id, &target, "ref-case", Some(&cref.raw))?;
        edges += 1;
    }
    counts.edges_written += edges;

    tx.commit()?;
    Ok(counts)
}

// ───────── Shared helpers ─────────

fn insert_edge(
    conn: &Connection,
    source_doc_id: i64,
    target_slug: &str,
    relation: &str,
    evidence: Option<&str>,
) -> Result<()> {
    let target_doc_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM vault_documents WHERE title = ?1",
            params![target_slug],
            |r| r.get::<_, i64>(0),
        )
        .ok();
    conn.execute(
        "INSERT INTO vault_links
            (source_doc_id, target_raw, target_doc_id, display_text,
             link_type, context, is_resolved)
         VALUES (?1, ?2, ?3, ?4, 'wikilink', ?5, ?6)",
        params![
            source_doc_id,
            target_slug,
            target_doc_id,
            relation,
            evidence,
            i32::from(target_doc_id.is_some()),
        ],
    )?;
    Ok(())
}

fn insert_tag(
    conn: &Connection,
    doc_id: i64,
    tag_name: &str,
    tag_type: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO vault_tags (doc_id, tag_name, tag_type) VALUES (?1, ?2, ?3)",
        params![doc_id, tag_name, tag_type],
    )?;
    Ok(())
}

fn hex_sha256(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn unix_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Scan unresolved links and attempt to resolve them by matching `target_raw`
/// against `vault_documents.title`. Returns the number of links newly resolved.
///
/// Call after each batch so edges pointing to nodes ingested later in the
/// same batch still wire up.
pub fn resolve_pending_links(conn: &Arc<Mutex<Connection>>) -> Result<usize> {
    let guard = conn.lock();
    let mut stmt = guard.prepare(
        "SELECT vl.id, vl.target_raw
           FROM vault_links vl
          WHERE vl.is_resolved = 0",
    )?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .filter_map(Result::ok)
        .collect();
    drop(stmt);
    let mut resolved = 0usize;
    for (link_id, target_raw) in rows {
        let target_id: Option<i64> = guard
            .query_row(
                "SELECT id FROM vault_documents WHERE title = ?1",
                params![target_raw],
                |r| r.get::<_, i64>(0),
            )
            .ok();
        if let Some(tid) = target_id {
            guard.execute(
                "UPDATE vault_links SET target_doc_id = ?1, is_resolved = 1 WHERE id = ?2",
                params![tid, link_id],
            )?;
            resolved += 1;
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::legal::{extract_case, extract_statute};
    use crate::vault::schema::init_schema;

    fn fresh_conn() -> Arc<Mutex<Connection>> {
        let c = Connection::open_in_memory().unwrap();
        init_schema(&c).unwrap();
        Arc::new(Mutex::new(c))
    }

    const STATUTE_MD: &str = r#"# 근로기준법

```json
{
  "meta": {"lsNm": "근로기준법", "ancYd": "20251001", "efYd": "20251001", "lsId": "001872", "lsiSeq": "276849"},
  "title": "근로기준법",
  "articles": [
    {"anchor": "J36:0", "number": "제36조(금품 청산)", "text": "제36조(금품 청산) 사용자는 ..."},
    {"anchor": "J109:0", "number": "제109조(벌칙)", "text": "제109조(벌칙) ① 제36조를 위반한 자는 처벌한다."}
  ],
  "supplements": []
}
```
"#;

    const CASE_MD: &str = r#"## 사건번호
2024노3424
## 선고일자
20250530
## 법원명
수원지법
## 사건명
근로기준법위반
## 참조조문
근로기준법 제36조, 제109조
## 참조판례

## 판시사항
요지
## 판결요지
요지
## 판례내용
본문
"#;

    #[test]
    fn ingest_statute_writes_article_rows_and_internal_edges() {
        let conn = fresh_conn();
        let doc = extract_statute(STATUTE_MD, "/x/20251001/근로기준법.md").unwrap();
        let c = ingest_statute(&conn, &doc).unwrap();
        assert_eq!(c.statute_articles_inserted, 2);
        assert_eq!(c.statute_articles_skipped_unchanged, 0);
        // Article 109 cites Article 36 → one internal-ref edge, resolved.
        let g = conn.lock();
        let resolved_count: i64 = g
            .query_row(
                "SELECT COUNT(*) FROM vault_links WHERE display_text='internal-ref' AND is_resolved=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resolved_count, 1);
    }

    #[test]
    fn ingest_statute_is_idempotent_on_unchanged_content() {
        let conn = fresh_conn();
        let doc = extract_statute(STATUTE_MD, "/x/20251001/근로기준법.md").unwrap();
        let first = ingest_statute(&conn, &doc).unwrap();
        let second = ingest_statute(&conn, &doc).unwrap();
        assert_eq!(first.statute_articles_inserted, 2);
        assert_eq!(second.statute_articles_skipped_unchanged, 2);
        assert_eq!(second.statute_articles_inserted, 0);
    }

    #[test]
    fn ingest_case_writes_cites_edges_and_resolves_against_statute() {
        let conn = fresh_conn();
        // Ingest statute first so case→statute edges resolve immediately.
        let sdoc = extract_statute(STATUTE_MD, "/x/20251001/근로기준법.md").unwrap();
        ingest_statute(&conn, &sdoc).unwrap();

        let cdoc = extract_case(CASE_MD, "/x/20250530/2024노3424_400102_형사_606941.md").unwrap();
        let counts = ingest_case(&conn, &cdoc).unwrap();
        assert_eq!(counts.cases_inserted, 1);
        assert!(counts.edges_written >= 2);

        let g = conn.lock();
        // Both 제36조 and 제109조 should be resolved cites edges.
        let resolved_cites: i64 = g
            .query_row(
                "SELECT COUNT(*) FROM vault_links WHERE display_text='cites' AND is_resolved=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resolved_cites, 2, "expected 2 resolved cites edges");
    }

    #[test]
    fn resolve_pending_links_fills_in_later_arrivals() {
        let conn = fresh_conn();
        // Ingest CASE first (statute not yet present) — cites should be unresolved.
        let cdoc = extract_case(CASE_MD, "/x/20250530/2024노3424_400102_형사_606941.md").unwrap();
        ingest_case(&conn, &cdoc).unwrap();
        {
            let g = conn.lock();
            let unresolved: i64 = g
                .query_row(
                    "SELECT COUNT(*) FROM vault_links WHERE is_resolved=0",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(unresolved >= 2);
        }
        // Now ingest the statute.
        let sdoc = extract_statute(STATUTE_MD, "/x/20251001/근로기준법.md").unwrap();
        ingest_statute(&conn, &sdoc).unwrap();
        // Post-batch resolve pass picks them up.
        let newly = resolve_pending_links(&conn).unwrap();
        assert!(newly >= 2, "expected ≥2 links resolved, got {newly}");
    }
}
