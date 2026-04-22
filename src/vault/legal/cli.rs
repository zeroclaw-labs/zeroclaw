//! `zeroclaw vault legal ingest <dir>` — walk a directory of markdown files,
//! classify each as statute or case, parse, and upsert into the vault.
//!
//! Safe to re-run: per-file checksum in `vault_documents` short-circuits
//! unchanged docs; changed docs replace their links, aliases, tags, and
//! frontmatter atomically in a transaction.

use super::{
    case_extractor::{extract_case, looks_like_case},
    ingest::{ingest_case, ingest_statute, resolve_pending_links, IngestCounts, IngestReport},
    statute_extractor::{extract_statute, looks_like_statute},
};
use crate::config::Config;
use anyhow::{Context, Result};
use console::style;
use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Walk a directory (or a single file), classify each markdown as statute
/// or case, and upsert into brain.db's vault tables. See the module docs
/// for write-layout semantics.
pub async fn ingest_path(config: &Config, root: PathBuf, dry_run: bool) -> Result<()> {
    if !root.exists() {
        anyhow::bail!("legal ingest: path does not exist: {}", root.display());
    }

    // Open (or create) brain.db at the conventional location.
    let db_path = config.workspace_dir.join("memory").join("brain.db");
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("creating memory dir {}", parent.display())
        })?;
    }

    let conn: Arc<Mutex<Connection>> = if dry_run {
        // Dry run: in-memory DB so we parse + would-insert but don't touch disk.
        let c = Connection::open_in_memory()?;
        crate::vault::schema::init_schema(&c)?;
        Arc::new(Mutex::new(c))
    } else {
        let c = Connection::open(&db_path)
            .with_context(|| format!("opening brain.db at {}", db_path.display()))?;
        crate::vault::schema::init_schema(&c)?;
        Arc::new(Mutex::new(c))
    };

    println!(
        "{} {}",
        style("legal ingest:").bold().cyan(),
        root.display()
    );
    if dry_run {
        println!(
            "{} (dry-run — using in-memory db, nothing persisted)",
            style("note:").yellow()
        );
    }

    let mut report = IngestReport::default();
    let files = collect_markdown_files(&root)?;
    for f in files {
        if let Err(e) = ingest_one(&conn, &f, &mut report).await {
            report
                .counts
                .errors
                .push(format!("{}: {e}", f.display()));
            eprintln!("{} {}: {e}", style("skip").yellow(), f.display());
        }
    }

    // Resolve pending edges (targets ingested later in this batch).
    let resolved_after = resolve_pending_links(&conn)?;
    report.counts.edges_resolved_after_pass += resolved_after;

    print_report(&report);
    Ok(())
}

async fn ingest_one(
    conn: &Arc<Mutex<Connection>>,
    path: &Path,
    report: &mut IngestReport,
) -> Result<()> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let source_path = path.to_string_lossy().to_string();

    // Route: case first (cheaper signal — explicit `## 사건번호`), fallback statute.
    if looks_like_case(&body) {
        let doc = extract_case(&body, &source_path)?;
        merge(&mut report.counts, ingest_case(conn, &doc)?);
    } else if looks_like_statute(&body) {
        let doc = extract_statute(&body, &source_path)?;
        merge(&mut report.counts, ingest_statute(conn, &doc)?);
    } else {
        anyhow::bail!("file matches neither case nor statute heuristics");
    }
    Ok(())
}

fn merge(dst: &mut IngestCounts, src: IngestCounts) {
    dst.statute_files += src.statute_files;
    dst.statute_articles_inserted += src.statute_articles_inserted;
    dst.statute_articles_skipped_unchanged += src.statute_articles_skipped_unchanged;
    dst.statute_articles_updated += src.statute_articles_updated;
    dst.case_files += src.case_files;
    dst.cases_inserted += src.cases_inserted;
    dst.cases_skipped_unchanged += src.cases_skipped_unchanged;
    dst.cases_updated += src.cases_updated;
    dst.edges_written += src.edges_written;
    // errors/resolved_after_pass are accumulated elsewhere.
}

fn collect_markdown_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    visit(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn visit(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if dir.is_file() {
        if is_markdown(dir) {
            out.push(dir.to_path_buf());
        }
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("reading directory {}", dir.display()))?
    {
        let entry = entry?;
        let p = entry.path();
        if p.is_dir() {
            // Skip hidden dirs (.git, .obsidian, etc.).
            if p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|s| s.starts_with('.'))
            {
                continue;
            }
            visit(&p, out)?;
        } else if is_markdown(&p) {
            out.push(p);
        }
    }
    Ok(())
}

fn is_markdown(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md"))
        .unwrap_or(false)
}

fn print_report(report: &IngestReport) {
    let c = &report.counts;
    println!();
    println!("{}", style("ingest report").bold());
    println!(
        "  statute files: {}   (articles: {} inserted, {} updated, {} unchanged)",
        c.statute_files,
        c.statute_articles_inserted,
        c.statute_articles_updated,
        c.statute_articles_skipped_unchanged,
    );
    println!(
        "  case files:    {}   ({} inserted, {} updated, {} unchanged)",
        c.case_files, c.cases_inserted, c.cases_updated, c.cases_skipped_unchanged,
    );
    println!(
        "  edges written: {}   (resolved-after pass: +{})",
        c.edges_written, c.edges_resolved_after_pass,
    );
    if !c.errors.is_empty() {
        println!(
            "  {} {} files with errors",
            style("warn:").yellow(),
            c.errors.len()
        );
        for e in c.errors.iter().take(10) {
            println!("    - {e}");
        }
        if c.errors.len() > 10 {
            println!("    (+{} more)", c.errors.len() - 10);
        }
    }
}

pub async fn stats(config: &Config) -> Result<()> {
    let db_path = config.workspace_dir.join("memory").join("brain.db");
    if !db_path.exists() {
        anyhow::bail!("no brain.db at {}", db_path.display());
    }
    let conn = Connection::open(&db_path)?;
    crate::vault::schema::init_schema(&conn)?;

    let statute_articles: i64 = conn.query_row(
        "SELECT COUNT(*) FROM vault_documents WHERE doc_type = 'statute_article'",
        [],
        |r| r.get(0),
    )?;
    let cases: i64 = conn.query_row(
        "SELECT COUNT(*) FROM vault_documents WHERE doc_type = 'case'",
        [],
        |r| r.get(0),
    )?;
    let edges: i64 = conn.query_row(
        "SELECT COUNT(*) FROM vault_links vl
           JOIN vault_documents d ON d.id = vl.source_doc_id
          WHERE d.doc_type IN ('statute_article','case')",
        [],
        |r| r.get(0),
    )?;
    let resolved: i64 = conn.query_row(
        "SELECT COUNT(*) FROM vault_links vl
           JOIN vault_documents d ON d.id = vl.source_doc_id
          WHERE d.doc_type IN ('statute_article','case') AND vl.is_resolved = 1",
        [],
        |r| r.get(0),
    )?;
    let distinct_laws: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT value) FROM vault_frontmatter WHERE key = 'law_name'",
        [],
        |r| r.get(0),
    )?;

    println!("{}", style("legal graph stats").bold());
    println!("  statute articles: {statute_articles}");
    println!("  distinct laws:    {distinct_laws}");
    println!("  cases:            {cases}");
    println!(
        "  edges:            {edges}  ({resolved} resolved, {} unresolved)",
        edges - resolved
    );
    Ok(())
}
