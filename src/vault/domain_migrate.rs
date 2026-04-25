//! Step A5 — `vault domain extract`.
//!
//! Migrates legal rows that landed in `brain.db` (e.g. from pre-v8 ingest
//! runs, or from a user accidentally invoking the old `ingest_statute`
//! shim) to `domain.db`. Source rows can be optionally **deleted** from
//! `brain.db` after a successful copy.
//!
//! Scope
//! ─────
//! Only legal `doc_type` rows are touched:
//!   `'statute_article' | 'statute_supplement' | 'statute_article_version' | 'case'`
//!
//! For every legal `vault_documents` row, this also copies the matching:
//!   - `vault_links`     (only edges originating from the legal row)
//!   - `vault_aliases`
//!   - `vault_frontmatter`
//!   - `vault_tags`
//!
//! Slug-conflict handling
//! ──────────────────────
//! When the same slug already exists in domain (e.g. previously baked
//! corpus), the source row is **skipped** and recorded in
//! `MigrationReport::skipped_slug_collision`. We never overwrite domain
//! rows from a brain-side copy — the assumption is the domain corpus is
//! the source of truth.
//!
//! Idempotency
//! ───────────
//! Re-running with `delete_source = false` is a no-op for already-copied
//! slugs. With `delete_source = true`, deletion uses checksum-equality
//! to ensure we only delete rows we actually copied successfully.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Arc;

const LEGAL_DOC_TYPES: &[&str] = &[
    "statute_article",
    "statute_supplement",
    "statute_article_version",
    "case",
];

#[derive(Debug, Default, Clone)]
pub struct MigrationReport {
    /// Distinct legal `vault_documents` slugs found in `brain.db`.
    pub source_legal_docs: usize,
    /// Slugs successfully copied to domain.
    pub copied: usize,
    /// Slugs already present in domain → skipped.
    pub skipped_slug_collision: usize,
    /// Source rows deleted from brain.db (only when `delete_source=true`).
    pub deleted_from_brain: usize,
    /// Auxiliary rows copied to domain (links + aliases + frontmatter + tags).
    pub aux_rows_copied: usize,
    /// Per-slug skip reason (capped at 50 to keep the report bounded).
    pub skipped_reasons: Vec<String>,
}

/// Migrate every legal row from `brain.db` into `domain.db` (creating
/// `domain.db` if absent). Returns a structured report. The two paths
/// must be different files; `brain_path == domain_path` is rejected.
///
/// `delete_source = true` removes the migrated rows from `brain.db`
/// after successful copy. Default `false` for safety — operators can
/// run a dry pass first, inspect the report, then re-run with
/// `--delete` once they are confident.
pub fn migrate_legal_to_domain(
    brain_path: &Path,
    domain_path: &Path,
    delete_source: bool,
) -> Result<MigrationReport> {
    if brain_path == domain_path {
        anyhow::bail!(
            "brain_path and domain_path must differ ({})",
            brain_path.display()
        );
    }
    if !brain_path.exists() {
        anyhow::bail!("brain.db not found: {}", brain_path.display());
    }
    super::domain::ensure_schema(domain_path)
        .with_context(|| format!("init domain schema at {}", domain_path.display()))?;

    // Open brain.db as the primary connection and ATTACH domain.db so
    // we can use cross-schema SQL inside a single transaction.
    let conn = Connection::open(brain_path)
        .with_context(|| format!("opening {}", brain_path.display()))?;
    super::schema::init_schema(&conn).context("init main schema (defensive)")?;
    super::domain::attach(&conn, domain_path)?;

    let conn = Arc::new(Mutex::new(conn));
    migrate_with_conn(&conn, delete_source)
}

/// Lower-level entry point used by tests with an existing Connection
/// that already has both `main` (brain) and `domain` schemas attached.
pub fn migrate_with_conn(
    conn: &Arc<Mutex<Connection>>,
    delete_source: bool,
) -> Result<MigrationReport> {
    let mut report = MigrationReport::default();

    // Pull every legal slug from main, with its checksum so we can
    // verify-then-delete safely.
    let placeholders = LEGAL_DOC_TYPES
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let select_sql = format!(
        "SELECT id, title, checksum FROM main.vault_documents
          WHERE doc_type IN ({placeholders})
            AND title IS NOT NULL
          ORDER BY id"
    );

    let mut guard = conn.lock();
    let tx = guard.transaction().context("starting migration transaction")?;

    let source_rows: Vec<(i64, String, String)> = {
        let mut stmt = tx.prepare(&select_sql)?;
        let params_dyn: Vec<&dyn rusqlite::ToSql> = LEGAL_DOC_TYPES
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        let rows = stmt.query_map(params_dyn.as_slice(), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.filter_map(Result::ok).collect()
    };
    report.source_legal_docs = source_rows.len();

    for (main_id, slug, source_checksum) in &source_rows {
        // Domain-side existence check — domain wins on slug collision.
        let domain_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM domain.vault_documents WHERE title = ?1",
                params![slug],
                |r| r.get(0),
            )
            .ok();
        if domain_id.is_some() {
            report.skipped_slug_collision += 1;
            if report.skipped_reasons.len() < 50 {
                report
                    .skipped_reasons
                    .push(format!("{slug}: already exists in domain"));
            }
            continue;
        }

        // Copy the document row + capture its new domain id.
        copy_document_row(&tx, *main_id)?;
        let new_domain_id: i64 = tx
            .query_row(
                "SELECT id FROM domain.vault_documents WHERE title = ?1",
                params![slug],
                |r| r.get(0),
            )
            .with_context(|| format!("post-copy lookup for slug {slug}"))?;

        // Auxiliary tables.
        report.aux_rows_copied += copy_aux_rows(&tx, *main_id, new_domain_id)?;
        report.copied += 1;

        if delete_source {
            // Verify checksum unchanged since SELECT to guarantee we
            // only delete what we actually copied.
            let cur: Option<String> = tx
                .query_row(
                    "SELECT checksum FROM main.vault_documents WHERE id = ?1",
                    params![main_id],
                    |r| r.get(0),
                )
                .ok();
            if cur.as_deref() == Some(source_checksum.as_str()) {
                delete_main_row(&tx, *main_id)?;
                report.deleted_from_brain += 1;
            }
        }
    }

    tx.commit().context("committing migration transaction")?;
    Ok(report)
}

fn copy_document_row(tx: &rusqlite::Transaction, main_id: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO domain.vault_documents
            (uuid, title, content, html_content, source_type, source_device_id,
             original_path, checksum, doc_type, char_count, created_at, updated_at,
             embedding_model, embedding_dim, embedding_provider,
             embedding_version, embedding_created_at)
         SELECT uuid, title, content, html_content, source_type, source_device_id,
                original_path, checksum, doc_type, char_count, created_at, updated_at,
                embedding_model, embedding_dim, embedding_provider,
                embedding_version, embedding_created_at
           FROM main.vault_documents WHERE id = ?1",
        params![main_id],
    )
    .context("copying vault_documents row to domain")?;
    Ok(())
}

fn copy_aux_rows(
    tx: &rusqlite::Transaction,
    main_id: i64,
    new_domain_id: i64,
) -> Result<usize> {
    let mut copied = 0usize;
    // vault_links — outbound only (this row as source). Inbound edges
    // belong to other source docs and are migrated when those rows
    // themselves migrate.
    copied += tx.execute(
        "INSERT INTO domain.vault_links
            (source_doc_id, target_raw, target_doc_id, display_text,
             link_type, context, line_number, is_resolved)
         SELECT ?1, target_raw, NULL, display_text,
                link_type, context, line_number, 0
           FROM main.vault_links WHERE source_doc_id = ?2",
        params![new_domain_id, main_id],
    )?;
    // vault_aliases.
    copied += tx.execute(
        "INSERT OR IGNORE INTO domain.vault_aliases (doc_id, alias)
         SELECT ?1, alias FROM main.vault_aliases WHERE doc_id = ?2",
        params![new_domain_id, main_id],
    )?;
    // vault_frontmatter.
    copied += tx.execute(
        "INSERT OR IGNORE INTO domain.vault_frontmatter (doc_id, key, value)
         SELECT ?1, key, value FROM main.vault_frontmatter WHERE doc_id = ?2",
        params![new_domain_id, main_id],
    )?;
    // vault_tags.
    copied += tx.execute(
        "INSERT OR IGNORE INTO domain.vault_tags (doc_id, tag_name, tag_type)
         SELECT ?1, tag_name, tag_type FROM main.vault_tags WHERE doc_id = ?2",
        params![new_domain_id, main_id],
    )?;
    // vault_embeddings (best-effort — table may not contain a row).
    copied += tx
        .execute(
            "INSERT OR IGNORE INTO domain.vault_embeddings (doc_id, embedding, dim)
             SELECT ?1, embedding, dim FROM main.vault_embeddings WHERE doc_id = ?2",
            params![new_domain_id, main_id],
        )
        .unwrap_or(0);
    Ok(copied)
}

fn delete_main_row(tx: &rusqlite::Transaction, main_id: i64) -> Result<()> {
    // Order matters: child rows first (FK-free schema, but logical clean).
    tx.execute("DELETE FROM main.vault_links WHERE source_doc_id = ?1", params![main_id])?;
    tx.execute("DELETE FROM main.vault_aliases WHERE doc_id = ?1", params![main_id])?;
    tx.execute("DELETE FROM main.vault_frontmatter WHERE doc_id = ?1", params![main_id])?;
    tx.execute("DELETE FROM main.vault_tags WHERE doc_id = ?1", params![main_id])?;
    tx.execute("DELETE FROM main.vault_embeddings WHERE doc_id = ?1", params![main_id])
        .ok();
    tx.execute("DELETE FROM main.vault_documents WHERE id = ?1", params![main_id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_attached() -> (Arc<Mutex<Connection>>, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let domain_path = tmp.path().join("domain.db");
        super::super::domain::ensure_schema(&domain_path).unwrap();
        let conn = Connection::open_in_memory().unwrap();
        super::super::schema::init_schema(&conn).unwrap();
        super::super::domain::attach(&conn, &domain_path).unwrap();
        (Arc::new(Mutex::new(conn)), tmp)
    }

    fn ingest_legal_into_main(conn: &Arc<Mutex<Connection>>) {
        let sdoc = crate::vault::legal::extract_statute(
            r#"# 근로기준법

```json
{
  "meta": {"lsNm": "근로기준법", "ancYd": "20251001", "efYd": "20251001"},
  "title": "근로기준법",
  "articles": [
    {"anchor": "J36:0", "number": "제36조(금품 청산)", "text": "제36조 14일 이내."},
    {"anchor": "J109:0", "number": "제109조(벌칙)", "text": "제109조 제36조를 위반한 자."}
  ],
  "supplements": []
}
```
"#,
            "/x/20251001/근로기준법.md",
        )
        .unwrap();
        crate::vault::legal::ingest_statute_to(
            conn,
            &sdoc,
            crate::vault::legal::IngestTarget::Main,
        )
        .unwrap();
    }

    #[test]
    fn migrate_copies_all_legal_rows_from_main_to_domain() {
        let (conn, _tmp) = fresh_attached();
        ingest_legal_into_main(&conn);

        // Pre-condition: rows live in main, not domain.
        {
            let g = conn.lock();
            let m: i64 = g
                .query_row(
                    "SELECT COUNT(*) FROM main.vault_documents \
                     WHERE doc_type='statute_article'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(m, 2);
            let d: i64 = g
                .query_row(
                    "SELECT COUNT(*) FROM domain.vault_documents",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(d, 0);
        }

        let report = migrate_with_conn(&conn, false).unwrap();
        assert_eq!(report.source_legal_docs, 2);
        assert_eq!(report.copied, 2);
        assert_eq!(report.skipped_slug_collision, 0);
        assert_eq!(report.deleted_from_brain, 0);
        assert!(report.aux_rows_copied > 0);

        // Post: domain now has both rows.
        let g = conn.lock();
        let d: i64 = g
            .query_row(
                "SELECT COUNT(*) FROM domain.vault_documents \
                 WHERE doc_type='statute_article'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(d, 2);
        // Source rows still present (delete_source=false).
        let m: i64 = g
            .query_row(
                "SELECT COUNT(*) FROM main.vault_documents \
                 WHERE doc_type='statute_article'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(m, 2);
    }

    #[test]
    fn migrate_with_delete_removes_source_rows() {
        let (conn, _tmp) = fresh_attached();
        ingest_legal_into_main(&conn);
        let report = migrate_with_conn(&conn, true).unwrap();
        assert_eq!(report.copied, 2);
        assert_eq!(report.deleted_from_brain, 2);

        let g = conn.lock();
        let m: i64 = g
            .query_row(
                "SELECT COUNT(*) FROM main.vault_documents \
                 WHERE doc_type='statute_article'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(m, 0, "source rows must be gone after --delete");
    }

    #[test]
    fn migrate_skips_slugs_already_in_domain() {
        let (conn, _tmp) = fresh_attached();
        // Pre-populate domain with the same slug that brain will hold.
        {
            let g = conn.lock();
            g.execute(
                "INSERT INTO domain.vault_documents
                    (uuid, title, content, source_type, source_device_id, checksum,
                     doc_type, char_count, created_at, updated_at)
                 VALUES ('u-d','statute::근로기준법::36','DOMAIN','local_file','local',
                         'cd','statute_article',6,1,1)",
                [],
            )
            .unwrap();
        }
        ingest_legal_into_main(&conn);

        let report = migrate_with_conn(&conn, true).unwrap();
        // 제36조 collides → skipped; 제109조 copies + deletes.
        assert_eq!(report.copied, 1);
        assert_eq!(report.skipped_slug_collision, 1);
        assert_eq!(report.deleted_from_brain, 1);

        // Domain's 제36조 content untouched.
        let g = conn.lock();
        let domain_36: String = g
            .query_row(
                "SELECT content FROM domain.vault_documents \
                 WHERE title='statute::근로기준법::36'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(domain_36, "DOMAIN");
        // Main's 제36조 still present (skipped, not copied → not deleted).
        let main_36: i64 = g
            .query_row(
                "SELECT COUNT(*) FROM main.vault_documents \
                 WHERE title='statute::근로기준법::36'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(main_36, 1, "skipped source row stays in brain");
    }

    #[test]
    fn migrate_is_idempotent_without_delete() {
        let (conn, _tmp) = fresh_attached();
        ingest_legal_into_main(&conn);
        let r1 = migrate_with_conn(&conn, false).unwrap();
        assert_eq!(r1.copied, 2);
        let r2 = migrate_with_conn(&conn, false).unwrap();
        // Second pass: every slug now already in domain → all skipped.
        assert_eq!(r2.copied, 0);
        assert_eq!(r2.skipped_slug_collision, 2);
    }

    #[test]
    fn migrate_rejects_same_path_for_brain_and_domain() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("only.db");
        // Need a file to exist for brain check.
        super::super::domain::ensure_schema(&p).unwrap();
        let err = migrate_legal_to_domain(&p, &p, false).unwrap_err();
        assert!(
            err.to_string().contains("must differ"),
            "got: {err}"
        );
    }

    #[test]
    fn migrate_carries_aliases_frontmatter_tags() {
        let (conn, _tmp) = fresh_attached();
        ingest_legal_into_main(&conn);
        migrate_with_conn(&conn, false).unwrap();
        let g = conn.lock();
        // Aliases like `근로기준법 제36조` must be findable in domain post-migration.
        let alias_count: i64 = g
            .query_row(
                "SELECT COUNT(*) FROM domain.vault_aliases va
                   JOIN domain.vault_documents d ON d.id = va.doc_id
                  WHERE va.alias = '근로기준법 제36조'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(alias_count >= 1, "alias must survive migration");
        // Tags also copied.
        let tag_count: i64 = g
            .query_row(
                "SELECT COUNT(*) FROM domain.vault_tags
                  WHERE tag_name = 'domain:legal'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(tag_count >= 2);
    }
}
