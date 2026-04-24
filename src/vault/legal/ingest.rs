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
use super::citation_patterns::extract_statute_citations;
use super::slug::{statute_aliases, supplement_slug};
use super::statute_extractor::{StatuteArticle, StatuteDoc, Supplement};
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
    pub supplements_inserted: usize,
    pub supplements_skipped_unchanged: usize,
    pub supplements_updated: usize,
    pub supplements_skipped_no_anc_no: usize,
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
    // 부칙 — each supplement becomes its own statute_supplement node so
    // it can be cited and cross-referenced independently of the main law.
    // Supplements without a parseable promulgation number are skipped
    // (they'd collide on slug) and recorded in the counts for visibility.
    for sup in &doc.supplements {
        match upsert_supplement(&tx, doc, sup)? {
            SupplementOutcome::Inserted(doc_id) => {
                counts.supplements_inserted += 1;
                counts.edges_written += write_supplement_edges(&tx, doc, sup, doc_id)?;
            }
            SupplementOutcome::Updated(doc_id) => {
                counts.supplements_updated += 1;
                counts.edges_written += write_supplement_edges(&tx, doc, sup, doc_id)?;
            }
            SupplementOutcome::SkippedUnchanged => {
                counts.supplements_skipped_unchanged += 1;
            }
            SupplementOutcome::SkippedNoAncNo => {
                counts.supplements_skipped_no_anc_no += 1;
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

enum SupplementOutcome {
    Inserted(i64),
    Updated(i64),
    SkippedUnchanged,
    /// Title had no parseable `법률 제N호` — we can't produce a stable slug
    /// so we skip. Count is reported so the operator knows coverage is
    /// incomplete.
    SkippedNoAncNo,
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

    // Amendment-date history parsed from inline `<개정 …>` / `<신설 …>` /
    // `[시행일 …]` tags inside the article body. The dates here signal
    // *when the article was last touched*; the corresponding 시행일 for
    // each amendment lives in the matching supplement (joined at query
    // time via promulgation_date).
    let amendment_dates = super::date_parse::extract_article_amendment_dates(&article.body);
    let amendment_dates_csv: Option<String> = if amendment_dates.is_empty() {
        None
    } else {
        Some(amendment_dates.join(","))
    };
    let latest_amendment = amendment_dates.last().cloned();

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
        ("amendment_dates", amendment_dates_csv),
        ("latest_amendment_date", latest_amendment),
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

fn upsert_supplement(
    conn: &Connection,
    doc: &StatuteDoc,
    sup: &Supplement,
) -> Result<SupplementOutcome> {
    let Some(anc_no) = sup.promulgation_no.as_deref() else {
        return Ok(SupplementOutcome::SkippedNoAncNo);
    };
    let slug = supplement_slug(&doc.law_name, anc_no);
    let content = format!("# {} — {}\n\n{}\n", doc.law_name, sup.title.trim(), sup.body);
    let checksum = hex_sha256(&content);
    let now = unix_epoch();

    let existing: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, checksum FROM vault_documents WHERE title = ?1",
            params![slug],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .ok();

    let doc_id: i64 = match existing {
        Some((_id, existing_sum)) if existing_sum == checksum => {
            return Ok(SupplementOutcome::SkippedUnchanged);
        }
        Some((id, _)) => {
            conn.execute(
                "UPDATE vault_documents
                   SET content = ?1, checksum = ?2, char_count = ?3, updated_at = ?4,
                       original_path = ?5, doc_type = 'statute_supplement',
                       source_type = 'local_file'
                 WHERE id = ?6",
                params![
                    content,
                    checksum,
                    content.chars().count() as i64,
                    now as i64,
                    doc.source_path,
                    id,
                ],
            )?;
            conn.execute(
                "DELETE FROM vault_links WHERE source_doc_id = ?1",
                params![id],
            )?;
            conn.execute(
                "DELETE FROM vault_frontmatter WHERE doc_id = ?1",
                params![id],
            )?;
            conn.execute("DELETE FROM vault_tags WHERE doc_id = ?1", params![id])?;
            conn.execute("DELETE FROM vault_aliases WHERE doc_id = ?1", params![id])?;
            id
        }
        None => {
            let uuid = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO vault_documents
                    (uuid, title, content, source_type, source_device_id, original_path,
                     checksum, doc_type, char_count, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'local_file', ?4, ?5, ?6, 'statute_supplement', ?7, ?8, ?8)",
                params![
                    uuid,
                    slug,
                    content,
                    "local",
                    doc.source_path,
                    checksum,
                    content.chars().count() as i64,
                    now as i64,
                ],
            )?;
            conn.last_insert_rowid()
        }
    };

    // Parse 시행일 (effective date) from the body. This is the date the
    // supplement's rules take effect, which per 행위시법 원칙 is the
    // date a practitioner matches against a 사건발생일 to decide which
    // version of a law applies.
    let effective_date = super::date_parse::parse_supplement_effective_date(
        &sup.body,
        sup.promulgation_date.as_deref(),
    );

    // Frontmatter: link back to parent law, preserve promulgation metadata,
    // and record effective_date (the operationally-important key).
    let fm: &[(&str, Option<String>)] = &[
        ("parent_law", Some(doc.law_name.clone())),
        ("kind", Some("supplement".to_string())),
        ("promulgation_no", Some(anc_no.to_string())),
        ("promulgation_date", sup.promulgation_date.clone()),
        ("effective_date", effective_date.clone()),
        ("supplement_note", sup.note.clone()),
        ("supplement_title", Some(sup.title.clone())),
    ];
    for (k, v) in fm {
        if let Some(val) = v {
            conn.execute(
                "INSERT OR IGNORE INTO vault_frontmatter (doc_id, key, value)
                 VALUES (?1, ?2, ?3)",
                params![doc_id, k, val],
            )?;
        }
    }

    // Aliases — humans cite supplements in a few recognisable ways.
    let alias_forms: Vec<String> = {
        let mut v = vec![
            format!("{} 부칙 <법률 제{}호>", doc.law_name, anc_no),
            format!("{} 부칙(법률 제{}호)", doc.law_name, anc_no),
        ];
        if let Some(d) = sup.promulgation_date.as_deref() {
            if d.len() == 8 {
                let pretty = format!("{}. {}. {}.", &d[..4], &d[4..6], &d[6..8]);
                v.push(format!("{} 부칙({})", doc.law_name, pretty));
            }
        }
        v
    };
    for alias in alias_forms {
        let _ = conn.execute(
            "INSERT OR IGNORE INTO vault_aliases (doc_id, alias) VALUES (?1, ?2)",
            params![doc_id, alias],
        );
    }

    // Tags.
    insert_tag(conn, doc_id, "domain:legal", Some("domain"))?;
    insert_tag(conn, doc_id, "kind:supplement", Some("kind"))?;
    insert_tag(conn, doc_id, &format!("law:{}", doc.law_name), Some("law"))?;
    if let Some(d) = sup.promulgation_date.as_deref() {
        if d.len() >= 4 {
            insert_tag(conn, doc_id, &format!("year:{}", &d[..4]), Some("year"))?;
        }
    }

    // Edge to the parent law's 제1조 as a lightweight "belongs-to" pointer
    // so agents can walk from a supplement to an article of its parent law
    // cheaply. We point at 제1조 (always present) rather than a synthetic
    // "law root" node.
    let parent_article1 = super::slug::statute_slug(&doc.law_name, 1, None);
    insert_edge(
        conn,
        doc_id,
        &parent_article1,
        "amends",
        Some(&sup.title),
    )?;

    match existing {
        Some(_) => Ok(SupplementOutcome::Updated(doc_id)),
        None => Ok(SupplementOutcome::Inserted(doc_id)),
    }
}

fn write_supplement_edges(
    conn: &Connection,
    doc: &StatuteDoc,
    sup: &Supplement,
    source_doc_id: i64,
) -> Result<usize> {
    // Run the same citation extractor over the supplement body; any
    // statute references inside (e.g. `제2조의4(업무위탁 등) …`) produce
    // edges just like regular articles. Current law is inherited for
    // bare `제N조` forms.
    let citations = extract_statute_citations(&sup.body, Some(&doc.law_name));
    let mut written = 0usize;
    for cite in &citations {
        let target = super::slug::statute_slug(&cite.law_name, cite.article, cite.article_sub);
        let relation = if cite.law_name == doc.law_name {
            "internal-ref"
        } else {
            "cross-law"
        };
        insert_edge(conn, source_doc_id, &target, relation, Some(&cite.raw))?;
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

    // 행위시법 원칙: extract every date literal from the case body so we
    // can later match 사건발생일 against statute 시행일. We scan both
    // the explicitly-marked 판례내용 section (if present, usually the
    // 판결이유 text) and the full markdown to catch dates in the
    // 판시사항 / 판결요지 summary blocks as well. Up to 50 dates are
    // retained to bound the stored CSV; earliest and latest are surfaced
    // separately for common range queries.
    let body_for_dates = doc
        .body
        .clone()
        .unwrap_or_else(|| doc.original_markdown.clone());
    let incident_dates = super::date_parse::find_all_dates(&body_for_dates);
    let incident_dates_capped: Vec<String> = incident_dates.iter().take(50).cloned().collect();

    // Fallback: Supreme / appellate judgments routinely omit explicit
    // incident dates because the 1st-instance judgment already carried
    // them. If no date literal was found, we infer the filing year from
    // referenced case numbers — the first 4 digits of a Korean case
    // number = 소장 접수 년도, which per 행위시법 원칙 aligns with the
    // applicable statute's 시행일. We store `{year}0101` / `{year}1231`
    // as sentinel earliest/latest so range queries still function, and
    // surface `incident_date_source` = `filing_year_fallback` so
    // consumers know the tolerance is ±1 year rather than exact.
    let (incident_dates_csv, incident_earliest, incident_latest, incident_source) =
        if !incident_dates_capped.is_empty() {
            (
                Some(incident_dates_capped.join(",")),
                incident_dates_capped.first().cloned(),
                incident_dates_capped.last().cloned(),
                "body",
            )
        } else {
            match super::date_parse::infer_filing_year_from_case_refs(
                &body_for_dates,
                &doc.case_number,
            ) {
                Some(year) => {
                    let earliest = format!("{year}0101");
                    let latest = format!("{year}1231");
                    (
                        Some(format!("{earliest},{latest}")),
                        Some(earliest),
                        Some(latest),
                        "filing_year_fallback",
                    )
                }
                None => (None, None, None, "none"),
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
        ("incident_dates", incident_dates_csv),
        ("incident_date_source", Some(incident_source.to_string())),
        ("incident_date_earliest", incident_earliest),
        ("incident_date_latest", incident_latest),
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

    const STATUTE_WITH_SUPPLEMENT: &str = r#"# 근로기준법

```json
{
  "meta": {"lsNm": "근로기준법", "ancYd": "20251001", "efYd": "20251001"},
  "title": "근로기준법",
  "articles": [
    {"anchor": "J1:0", "number": "제1조(목적)", "text": "제1조(목적) 본법의 목적."}
  ],
  "supplements": [
    {"title": "부칙  <법률 제21065호, 2025. 10. 1.>   (정부조직법)",
     "body": "제8조 생략"},
    {"title": "부칙 (오래된 형식)",
     "body": "내용"}
  ]
}
```
"#;

    #[test]
    fn ingest_statute_creates_supplement_nodes_with_parsed_metadata() {
        let conn = fresh_conn();
        let doc =
            extract_statute(STATUTE_WITH_SUPPLEMENT, "/x/20251001/근로기준법.md").unwrap();
        let c = ingest_statute(&conn, &doc).unwrap();
        assert_eq!(c.statute_articles_inserted, 1);
        assert_eq!(c.supplements_inserted, 1, "one parseable supplement");
        assert_eq!(
            c.supplements_skipped_no_anc_no, 1,
            "one legacy-format supplement without anc_no must be skipped"
        );

        let g = conn.lock();
        // Supplement node exists under the promulgation-number slug.
        let sup_id: i64 = g
            .query_row(
                "SELECT id FROM vault_documents WHERE title = 'statute::근로기준법::supplement::21065'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Frontmatter carries the parsed pieces.
        let parent_law: String = g
            .query_row(
                "SELECT value FROM vault_frontmatter WHERE doc_id = ?1 AND key='parent_law'",
                [sup_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parent_law, "근로기준법");
        let prom_date: String = g
            .query_row(
                "SELECT value FROM vault_frontmatter WHERE doc_id = ?1 AND key='promulgation_date'",
                [sup_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(prom_date, "20251001");
    }

    #[test]
    fn ingest_statute_supplement_is_idempotent() {
        let conn = fresh_conn();
        let doc =
            extract_statute(STATUTE_WITH_SUPPLEMENT, "/x/20251001/근로기준법.md").unwrap();
        let first = ingest_statute(&conn, &doc).unwrap();
        let second = ingest_statute(&conn, &doc).unwrap();
        assert_eq!(first.supplements_inserted, 1);
        assert_eq!(second.supplements_skipped_unchanged, 1);
        assert_eq!(second.supplements_inserted, 0);
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

    const STATUTE_WITH_SUPPLEMENT_MD: &str = r#"# 근로기준법

```json
{
  "meta": {"lsNm": "근로기준법", "ancYd": "20251001", "efYd": "20251001"},
  "title": "근로기준법",
  "articles": [
    {"anchor": "J2:0", "number": "제2조(정의)",
     "text": "제2조(정의) ① 정의 <개정 2018. 3. 20., 2019. 1. 15., 2020. 5. 26.>"},
    {"anchor": "J36:0", "number": "제36조(금품 청산)",
     "text": "제36조(금품 청산) 지급한다. <개정 2020. 5. 26.>"}
  ],
  "supplements": [
    {"title": "부칙 <법률 제21065호, 2025. 10. 1.>",
     "body": "제1조(시행일) 이 법은 공포한 날부터 시행한다."}
  ]
}
```
"#;

    #[test]
    fn supplement_stores_effective_date_and_falls_back_to_promulgation() {
        let conn = fresh_conn();
        let doc = extract_statute(
            STATUTE_WITH_SUPPLEMENT_MD,
            "/x/20251001/근로기준법.md",
        )
        .unwrap();
        ingest_statute(&conn, &doc).unwrap();
        let g = conn.lock();
        let eff: String = g
            .query_row(
                "SELECT value FROM vault_frontmatter
                   WHERE key = 'effective_date'
                     AND doc_id = (SELECT id FROM vault_documents
                                    WHERE title = 'statute::근로기준법::supplement::21065')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // `공포한 날부터 시행` → effective = promulgation = 20251001.
        assert_eq!(eff, "20251001");
    }

    #[test]
    fn article_stores_amendment_dates_csv_and_latest() {
        let conn = fresh_conn();
        let doc = extract_statute(
            STATUTE_WITH_SUPPLEMENT_MD,
            "/x/20251001/근로기준법.md",
        )
        .unwrap();
        ingest_statute(&conn, &doc).unwrap();
        let g = conn.lock();
        // 제2조 has three amendment dates — all three in CSV, latest = 20200526.
        let csv: String = g
            .query_row(
                "SELECT value FROM vault_frontmatter
                   WHERE key = 'amendment_dates'
                     AND doc_id = (SELECT id FROM vault_documents
                                    WHERE title = 'statute::근로기준법::2')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(csv, "20180320,20190115,20200526");
        let latest: String = g
            .query_row(
                "SELECT value FROM vault_frontmatter
                   WHERE key = 'latest_amendment_date'
                     AND doc_id = (SELECT id FROM vault_documents
                                    WHERE title = 'statute::근로기준법::2')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(latest, "20200526");
    }

    #[test]
    fn case_stores_incident_dates_from_body() {
        let conn = fresh_conn();
        let case_md = r#"## 사건번호
2024노3424
## 선고일자
20250530
## 법원명
수원지법
## 사건명
근로기준법위반
## 참조조문
근로기준법 제36조
## 판시사항
test
## 판결요지
test
## 판례내용
피고인과 甲은 2024. 3. 28. 임의조정을 성립하였고, 2024. 4. 5. 800만 원을 지급하였다. 이후 2025. 4. 10. 탄원서를 제출하였다.
"#;
        let cdoc =
            extract_case(case_md, "/x/20250530/2024노3424_400102_형사_606941.md").unwrap();
        ingest_case(&conn, &cdoc).unwrap();
        let g = conn.lock();
        let earliest: String = g
            .query_row(
                "SELECT value FROM vault_frontmatter
                   WHERE key = 'incident_date_earliest'
                     AND doc_id = (SELECT id FROM vault_documents
                                    WHERE title = 'case::2024노3424')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(earliest, "20240328");
        let latest: String = g
            .query_row(
                "SELECT value FROM vault_frontmatter
                   WHERE key = 'incident_date_latest'
                     AND doc_id = (SELECT id FROM vault_documents
                                    WHERE title = 'case::2024노3424')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(latest, "20250410");
        // CSV must contain all three in sorted order.
        let csv: String = g
            .query_row(
                "SELECT value FROM vault_frontmatter
                   WHERE key = 'incident_dates'
                     AND doc_id = (SELECT id FROM vault_documents
                                    WHERE title = 'case::2024노3424')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(csv, "20240328,20240405,20250410");
    }
}
