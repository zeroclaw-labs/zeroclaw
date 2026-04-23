//! `zeroclaw vault legal ingest <dir>` — walk a directory of markdown files,
//! classify each as statute or case, parse, and upsert into the vault.
//!
//! Safe to re-run: per-file checksum in `vault_documents` short-circuits
//! unchanged docs; changed docs replace their links, aliases, tags, and
//! frontmatter atomically in a transaction.

use super::{
    case_extractor::{extract_case, looks_like_case},
    graph_query::{self, NodeKind, Subgraph},
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
    // Accept UTF-8 / UTF-8 w/ BOM / UTF-16 / CP949 / EUC-KR — Korean legal
    // corpora arrive in a mix of encodings depending on vintage. We log
    // anything that wasn't vanilla UTF-8 so operators can spot malformed
    // dumps in the `skip` stream.
    let decoded = super::encoding::read_markdown_auto(path)
        .with_context(|| format!("reading {}", path.display()))?;
    if decoded.encoding != "utf-8" {
        tracing::debug!(
            path = %path.display(),
            encoding = decoded.encoding,
            had_errors = decoded.had_errors,
            "legal ingest: non-UTF-8 input decoded"
        );
    }
    if decoded.had_errors {
        tracing::warn!(
            path = %path.display(),
            encoding = decoded.encoding,
            "legal ingest: decoder produced replacement characters — file may be a different encoding"
        );
    }
    let body = decoded.content;
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
    dst.supplements_inserted += src.supplements_inserted;
    dst.supplements_skipped_unchanged += src.supplements_skipped_unchanged;
    dst.supplements_updated += src.supplements_updated;
    dst.supplements_skipped_no_anc_no += src.supplements_skipped_no_anc_no;
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
        "  supplements:   {} inserted, {} updated, {} unchanged, {} skipped (no 공포번호)",
        c.supplements_inserted,
        c.supplements_updated,
        c.supplements_skipped_unchanged,
        c.supplements_skipped_no_anc_no,
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

/// Export format for `zeroclaw vault legal export`.
pub enum ExportFormat {
    /// Single self-contained HTML file — Cytoscape viewer with the subgraph
    /// embedded inline. Works offline, no network needed.
    Html,
    /// graphify-compatible JSON (`{nodes, edges, __meta}`) for external tools.
    Json,
}

/// Compute a subgraph rooted at `root_slug` up to `depth` hops, filtered by
/// `kinds` (comma-separated `statute,case`), and write it to `out` as either
/// a standalone HTML viewer or raw JSON.
pub fn export_subgraph(
    config: &Config,
    root: &str,
    depth: usize,
    kinds: Option<&str>,
    format: ExportFormat,
    out: &Path,
) -> Result<()> {
    let db_path = config.workspace_dir.join("memory").join("brain.db");
    if !db_path.exists() {
        anyhow::bail!("brain.db not found at {}", db_path.display());
    }
    let conn = Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening {}", db_path.display()))?;

    let kinds_parsed = parse_kinds_csv(kinds);
    let sg = graph_query::neighbors(&conn, root, depth, &kinds_parsed)?;
    let exported_at = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z").to_string();

    // Augment with export metadata so the viewer can show provenance.
    let mut sg_value = serde_json::to_value(&sg)?;
    if let Some(obj) = sg_value.as_object_mut() {
        obj.insert(
            "__meta".to_string(),
            serde_json::json!({
                "root": root,
                "depth": depth,
                "kinds": kinds,
                "exported_at": exported_at,
                "source": "zeroclaw vault legal export",
            }),
        );
    }
    let sg_json = serde_json::to_string(&sg_value)?;

    match format {
        ExportFormat::Json => {
            std::fs::write(out, sg_json.as_bytes())
                .with_context(|| format!("writing {}", out.display()))?;
        }
        ExportFormat::Html => {
            let html = build_snapshot_html(&sg_json);
            std::fs::write(out, html.as_bytes())
                .with_context(|| format!("writing {}", out.display()))?;
        }
    }

    println!(
        "{} {} (nodes={}, edges={}{}) → {}",
        style("wrote").green(),
        match format {
            ExportFormat::Html => "HTML snapshot",
            ExportFormat::Json => "JSON subgraph",
        },
        sg.nodes.len(),
        sg.edges.len(),
        if sg.truncated { ", TRUNCATED" } else { "" },
        out.display()
    );
    Ok(())
}

fn parse_kinds_csv(raw: Option<&str>) -> Vec<NodeKind> {
    let Some(raw) = raw else {
        return vec![];
    };
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| match s {
            "statute" => Some(NodeKind::Statute),
            "case" => Some(NodeKind::Case),
            _ => None,
        })
        .collect()
}

/// The HTML template is the same Cytoscape viewer served by the gateway.
/// The exporter injects the subgraph as a `<script type="application/json"
/// id="__prebundled_subgraph__">...</script>` tag right before `</body>`;
/// the viewer's bootstrap sees it and renders without any network calls.
fn build_snapshot_html(subgraph_json: &str) -> String {
    // Escape `</script>` within the JSON payload so it can't terminate the
    // embedding script tag.
    let safe_json = subgraph_json.replace("</", "<\\/");
    let mut html = VIEWER_TEMPLATE.to_string();
    let injection = format!(
        "<script type=\"application/json\" id=\"__prebundled_subgraph__\">{safe_json}</script>\n</body>"
    );
    // Replace the final `</body>` (case-sensitive, matches what we ship).
    if let Some(idx) = html.rfind("</body>") {
        html.replace_range(idx..idx + "</body>".len(), &injection);
    } else {
        // Fallback — append; unlikely, template is stable.
        html.push_str(&injection);
    }
    html
}

const VIEWER_TEMPLATE: &str = include_str!("../../gateway/legal_graph_viewer.html");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_injects_prebundle_script_before_body_close() {
        let json = r#"{"nodes":[],"edges":[],"__meta":{"root":"x"}}"#;
        let html = build_snapshot_html(json);
        assert!(
            html.contains(r#"<script type="application/json" id="__prebundled_subgraph__">"#),
            "prebundle script tag missing"
        );
        // The injected block must sit before `</body>`.
        let close = html.rfind("</body>").expect("viewer must have </body>");
        let inject = html
            .find("__prebundled_subgraph__")
            .expect("injection marker");
        assert!(inject < close, "prebundle must come before </body>");
        // JSON payload round-trips.
        assert!(html.contains(r#""root":"x""#));
    }

    #[test]
    fn snapshot_escapes_script_close_tag_in_payload() {
        // A malicious payload containing `</script>` must not break out of the
        // embedding tag. We escape `</` → `<\/` which keeps JSON.parse happy
        // but prevents HTML parser from terminating the script.
        let json = r#"{"evil":"</script><img src=x onerror=alert(1)>"}"#;
        let html = build_snapshot_html(json);
        assert!(
            !html.contains("</script><img"),
            "script tag termination not escaped: {html}"
        );
        assert!(
            html.contains(r#"<\/script><img"#) || html.contains(r#"<\/"#),
            "expected `<\\/` escape in the payload"
        );
    }

    #[test]
    fn parse_kinds_csv_filters_unknown() {
        use NodeKind::*;
        assert_eq!(parse_kinds_csv(None), Vec::<NodeKind>::new());
        assert_eq!(parse_kinds_csv(Some("")), Vec::<NodeKind>::new());
        assert_eq!(parse_kinds_csv(Some("statute")), vec![Statute]);
        assert_eq!(parse_kinds_csv(Some("case, statute, bogus")), vec![Case, Statute]);
    }

    #[tokio::test]
    async fn embed_bails_cleanly_when_no_provider_configured() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut cfg = crate::config::Config::default();
        cfg.workspace_dir = tmp.path().to_path_buf();
        cfg.memory.embedding_provider = "none".to_string();
        let err = embed_legal(&cfg, None, 8).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("embedding_provider") || msg.contains("embedding provider"),
            "error should mention missing embedding provider: {msg}"
        );
    }
}

/// `zeroclaw vault legal embed` — populate `vault_embeddings` rows for
/// every legal node that doesn't yet have one. Uses the memory config's
/// embedding provider (`config.memory.embedding_provider/model/dimensions`)
/// plus the resolved API key pulled from the configured provider chain.
///
/// Safe to re-run: documents already present in `vault_embeddings` are
/// skipped. Also re-embeds if `vault_documents.embedding_version` differs
/// from the provider's current version (schema drift).
pub async fn embed_legal(config: &Config, limit: Option<usize>, batch: usize) -> Result<()> {
    use crate::memory::embedding::{create_embedding_provider, EmbeddingProvider};
    use crate::memory::vector::vec_to_bytes;

    let batch = batch.max(1).min(32);

    let provider_name = config.memory.embedding_provider.clone();
    if provider_name == "none" || provider_name.is_empty() {
        anyhow::bail!(
            "no embedding provider configured — set `memory.embedding_provider` to `openai` / \
             `openrouter` / `custom:<url>` / `local_fastembed` before running `vault legal embed`"
        );
    }
    // Embedding API key resolution (in order of specificity):
    //   1. `OPENAI_API_KEY` env var (standard cloud deployment)
    //   2. The top-level `config.api_key` (shared chat+embedding key)
    //   3. Any per-provider key in `config.model_providers`
    // local_fastembed + custom:URL don't need a key; they can tolerate None.
    let api_key = std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .or_else(|| config.api_key.clone().filter(|k| !k.trim().is_empty()))
        .or_else(|| {
            config
                .model_providers
                .values()
                .find_map(|p| p.api_key.clone().filter(|k| !k.trim().is_empty()))
        });

    let provider: Box<dyn EmbeddingProvider> = create_embedding_provider(
        &provider_name,
        api_key.as_deref(),
        &config.memory.embedding_model,
        config.memory.embedding_dimensions,
    );
    let dims = provider.dimensions();
    if dims == 0 {
        anyhow::bail!(
            "embedding provider `{provider_name}` reported dim=0 — check the provider feature \
             flag (e.g. `--features embedding-local`) or the provider config"
        );
    }

    let db_path = config.workspace_dir.join("memory").join("brain.db");
    if !db_path.exists() {
        anyhow::bail!("no brain.db at {}", db_path.display());
    }

    println!(
        "{} provider: {} / model: {} / dims: {}",
        style("embedding legal nodes").bold(),
        provider.name(),
        provider.model(),
        dims
    );

    // Pull work list: legal docs that either have no vault_embeddings row
    // or whose recorded embedding schema version is stale.
    let conn = Connection::open(&db_path)?;
    crate::vault::schema::init_schema(&conn)?;
    let cur_version = provider.version() as i64;
    let limit_sql = limit.unwrap_or(usize::MAX) as i64;
    let mut stmt = conn.prepare(
        "SELECT d.id, d.content
           FROM vault_documents d
           LEFT JOIN vault_embeddings e ON e.doc_id = d.id
          WHERE d.doc_type IN ('statute_article','statute_supplement','case')
            AND (
                e.doc_id IS NULL
             OR COALESCE(d.embedding_version, 0) <> ?1
            )
          LIMIT ?2",
    )?;
    let work: Vec<(i64, String)> = stmt
        .query_map(rusqlite::params![cur_version, limit_sql], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .filter_map(Result::ok)
        .collect();
    drop(stmt);

    if work.is_empty() {
        println!("nothing to do — all legal nodes already embedded at version {cur_version}");
        return Ok(());
    }

    println!("  {} nodes to embed (batch size {batch})", work.len());
    let total = work.len();
    let mut done = 0usize;
    let mut embedded = 0usize;
    let mut skipped_empty = 0usize;

    for chunk in work.chunks(batch) {
        let texts: Vec<&str> = chunk.iter().map(|(_id, c)| c.as_str()).collect();
        let vectors = provider.embed(&texts).await.with_context(|| {
            format!("embedding batch of {} texts failed", texts.len())
        })?;
        for ((doc_id, _content), vec) in chunk.iter().zip(vectors.iter()) {
            if vec.is_empty() {
                skipped_empty += 1;
                continue;
            }
            let bytes = vec_to_bytes(vec);
            // Upsert embedding row + update the doc's schema-version fields.
            conn.execute(
                "INSERT INTO vault_embeddings (doc_id, embedding, dim)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(doc_id) DO UPDATE SET embedding = excluded.embedding,
                                                   dim = excluded.dim,
                                                   created_at = unixepoch()",
                rusqlite::params![doc_id, bytes, vec.len() as i64],
            )?;
            conn.execute(
                "UPDATE vault_documents
                    SET embedding_model = ?1,
                        embedding_dim = ?2,
                        embedding_provider = ?3,
                        embedding_version = ?4,
                        embedding_created_at = unixepoch()
                  WHERE id = ?5",
                rusqlite::params![
                    provider.model(),
                    vec.len() as i64,
                    provider.name(),
                    cur_version,
                    doc_id,
                ],
            )?;
            embedded += 1;
        }
        done += chunk.len();
        if total > batch {
            println!(
                "  {} / {} ({:>3}%)",
                done,
                total,
                (done * 100).checked_div(total).unwrap_or(100)
            );
        }
    }

    println!(
        "{} embedded {embedded} / skipped {skipped_empty} (empty vectors)",
        style("done").green().bold()
    );
    Ok(())
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
    let supplements: i64 = conn.query_row(
        "SELECT COUNT(*) FROM vault_documents WHERE doc_type = 'statute_supplement'",
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
          WHERE d.doc_type IN ('statute_article','statute_supplement','case')",
        [],
        |r| r.get(0),
    )?;
    let resolved: i64 = conn.query_row(
        "SELECT COUNT(*) FROM vault_links vl
           JOIN vault_documents d ON d.id = vl.source_doc_id
          WHERE d.doc_type IN ('statute_article','statute_supplement','case') AND vl.is_resolved = 1",
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
    println!("  supplements:      {supplements}");
    println!("  distinct laws:    {distinct_laws}");
    println!("  cases:            {cases}");
    println!(
        "  edges:            {edges}  ({resolved} resolved, {} unresolved)",
        edges - resolved
    );
    Ok(())
}
