//! Read-only graph queries over `vault_documents` + `vault_links`, filtered
//! to legal nodes (`doc_type IN ('statute_article','statute_supplement','case')`).
//!
//! Shared by:
//!   - Phase 2 agent tools in `src/tools/vault_graph.rs`
//!   - Phase 3 HTTP endpoint in `src/vault/legal/graph_http.rs`
//!   - Phase 4 static snapshot exporter
//!
//! Traversal is **iterative BFS over resolved edges**, not SQL recursive CTE.
//! Rationale: depth limits + cycle handling + per-kind filtering are clearer
//! in Rust, and the edge counts we expect (a few thousand at most for even
//! a sizeable law-corpus subtree) are well within in-memory reach.
//!
//! All queries respect a hard **node cap** (`MAX_NODES`, 500) so agents
//! can't accidentally blow the response token budget with a depth-10
//! walk that hits every statute.
//!
//! Cross-DB resolution (Step A4)
//! ─────────────────────────────
//! Reads transparently UNION across `main` (brain.db) and `domain`
//! (domain.db, when ATTACHed). On slug conflicts the **domain** row
//! wins — domain-shipped corpora take precedence over user-pasted
//! content sharing the same slug. ID-based subqueries (frontmatter,
//! tags, aliases) are scoped to the schema where the row was found,
//! since SQLite IDs are per-schema. Edges follow slugs only — never
//! cached integer FKs across schemas — so swapping `domain.db` cannot
//! leave dangling references in `main.vault_links`.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

/// Resolution priority: domain rows shadow main rows on slug conflict.
/// Order is fixed (`domain`, `main`); `main` is always present, `domain`
/// only when ATTACHed. The schema name is the fully-qualified SQL
/// prefix (e.g. `domain.vault_documents`).
const SCHEMAS_BY_PRIORITY: [&str; 2] = ["domain", "main"];

fn domain_attached(conn: &Connection) -> bool {
    super::super::domain::is_attached(conn).unwrap_or(false)
}

/// Schemas to read from this query, ordered by priority (domain first
/// when attached, then main). Domain-priority means earlier entries
/// shadow later ones when slugs collide.
fn active_schemas(conn: &Connection) -> &'static [&'static str] {
    if domain_attached(conn) {
        &SCHEMAS_BY_PRIORITY
    } else {
        &SCHEMAS_BY_PRIORITY[1..]
    }
}

/// Look up a slug across all active schemas, returning the highest-priority
/// hit. Returns `(schema, id, doc_type)` so subsequent ID-based queries
/// can stay scoped to the schema where the row lives.
fn lookup_slug_in_legal(
    conn: &Connection,
    slug: &str,
) -> Result<Option<(&'static str, i64, String)>> {
    for schema in active_schemas(conn) {
        let row: Option<(i64, String)> = conn
            .query_row(
                &format!(
                    "SELECT id, doc_type FROM {schema}.vault_documents
                       WHERE title = ?1
                         AND doc_type IN
                            ('statute_article','statute_supplement',
                             'statute_article_version','case')"
                ),
                params![slug],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .ok();
        if let Some((id, doc_type)) = row {
            return Ok(Some((schema, id, doc_type)));
        }
    }
    Ok(None)
}

/// Hard cap on nodes returned from any single traversal call.
pub const MAX_NODES: usize = 500;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Statute,
    Case,
}

impl NodeKind {
    /// Map the `vault_documents.doc_type` string into our kind enum.
    /// Both `statute_article` and `statute_supplement` map to
    /// `Statute` — supplements are a sub-flavour of statute content
    /// for graph-query purposes; callers that care about the
    /// article/supplement distinction can check the `kind` frontmatter
    /// entry (`"supplement"` for 부칙, absent / `"article"` otherwise).
    pub fn from_doc_type(s: &str) -> Option<Self> {
        match s {
            "statute_article" | "statute_supplement" | "statute_article_version" => {
                Some(Self::Statute)
            }
            "case" => Some(Self::Case),
            _ => None,
        }
    }
    pub fn as_doc_type(self) -> &'static str {
        match self {
            Self::Statute => "statute_article",
            Self::Case => "case",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: i64,
    /// Canonical slug (= vault_documents.title).
    pub slug: String,
    pub kind: NodeKind,
    /// Human label for UI — law+article header for statutes, case name for cases.
    pub label: String,
    // Statute-only structured metadata (from vault_frontmatter).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub law_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub article_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub article_title_kw: Option<String>,
    // Case-only metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub case_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub court_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub case_name: Option<String>,
    /// 행위시법 원칙 alignment fields — populated when the corresponding
    /// extractor produced a value. Shape depends on node subtype:
    ///
    /// - statute article → `latest_amendment_date`
    /// - statute supplement → `effective_date` (+ `promulgation_date`)
    /// - case → `incident_date_earliest` / `incident_date_latest`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promulgation_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_amendment_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incident_date_earliest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incident_date_latest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub source_slug: String,
    pub target_slug: String,
    /// `cites` / `ref-case` / `internal-ref` / `cross-law` — from `vault_links.display_text`.
    pub relation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    pub resolved: bool,
    /// Highest-priority applicable-law signal — populated when the
    /// source citation was a `구 {법}(YYYY. M. D. 개정되기 전의 것)`
    /// form. YYYYMMDD string; the applicable law version is whichever
    /// effective_date ≤ this date - 1 day.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision_cutoff: Option<String>,
}

/// Graphify-compatible subgraph JSON shape:
/// `{ "nodes": [...], "edges": [...] }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subgraph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// True if traversal hit [`MAX_NODES`] and returned partial results.
    #[serde(default)]
    pub truncated: bool,
}

/// Load a single node by slug. Returns `None` if the slug isn't present or
/// its `doc_type` isn't in the legal set. Domain-priority: when the same
/// slug exists in both `main` and `domain`, the `domain` row wins.
pub fn get_node(conn: &Connection, slug: &str) -> Result<Option<Node>> {
    let Some((schema, id, doc_type)) = lookup_slug_in_legal(conn, slug)? else {
        return Ok(None);
    };
    let kind = NodeKind::from_doc_type(&doc_type).unwrap();
    Ok(Some(hydrate_node(conn, schema, id, slug, kind)?))
}

/// Fetch `depth`-hop neighbors of `root_slug` as a subgraph. Traverses both
/// directions (outbound `source→target` and inbound `target←source`).
///
/// `kinds_filter`: if non-empty, only include nodes whose kind is in this
/// set. Edges are kept only when both endpoints are in the kept set.
pub fn neighbors(
    conn: &Connection,
    root_slug: &str,
    depth: usize,
    kinds_filter: &[NodeKind],
) -> Result<Subgraph> {
    let root = get_node(conn, root_slug)?
        .with_context(|| format!("node not found: {root_slug}"))?;
    if !pass_kinds(kinds_filter, root.kind) {
        return Ok(Subgraph {
            nodes: vec![],
            edges: vec![],
            truncated: false,
        });
    }

    let mut visited: HashMap<String, Node> = HashMap::new();
    visited.insert(root.slug.clone(), root.clone());

    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((root.slug.clone(), 0));

    let mut truncated = false;
    while let Some((slug, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        if visited.len() >= MAX_NODES {
            truncated = true;
            break;
        }
        // Walk both directions.
        let neighbor_slugs = fetch_direct_neighbors(conn, &slug)?;
        for ns in neighbor_slugs {
            if visited.contains_key(&ns) {
                continue;
            }
            let Some(n) = get_node(conn, &ns)? else {
                continue;
            };
            if !pass_kinds(kinds_filter, n.kind) {
                continue;
            }
            visited.insert(n.slug.clone(), n);
            queue.push_back((ns.clone(), d + 1));
            if visited.len() >= MAX_NODES {
                truncated = true;
                break;
            }
        }
    }

    let edges = fetch_edges_within(conn, &visited.keys().cloned().collect::<Vec<_>>())?;

    let mut nodes: Vec<Node> = visited.into_values().collect();
    nodes.sort_by(|a, b| a.slug.cmp(&b.slug));

    Ok(Subgraph {
        nodes,
        edges,
        truncated,
    })
}

/// Shortest undirected path (edge-count) from `from_slug` to `to_slug`, up to
/// `max_depth` hops. Returns the slug sequence including both endpoints, or
/// `None` if no path within the bound.
pub fn shortest_path(
    conn: &Connection,
    from_slug: &str,
    to_slug: &str,
    max_depth: usize,
) -> Result<Option<Vec<String>>> {
    if from_slug == to_slug {
        return Ok(Some(vec![from_slug.to_string()]));
    }
    if get_node(conn, from_slug)?.is_none() || get_node(conn, to_slug)?.is_none() {
        return Ok(None);
    }

    let mut parents: HashMap<String, String> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(from_slug.to_string());
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((from_slug.to_string(), 0));

    while let Some((slug, d)) = queue.pop_front() {
        if d >= max_depth {
            continue;
        }
        for ns in fetch_direct_neighbors(conn, &slug)? {
            if visited.contains(&ns) {
                continue;
            }
            visited.insert(ns.clone());
            parents.insert(ns.clone(), slug.clone());
            if ns == to_slug {
                // Reconstruct path.
                let mut path = vec![to_slug.to_string()];
                let mut cur = to_slug.to_string();
                while let Some(p) = parents.get(&cur) {
                    path.push(p.clone());
                    cur = p.clone();
                }
                path.reverse();
                return Ok(Some(path));
            }
            queue.push_back((ns, d + 1));
        }
    }
    Ok(None)
}

/// Return the subgraph induced by the given set of slugs (all edges between
/// them, both resolved and unresolved). Useful for "extract" / export flows.
pub fn induced_subgraph(conn: &Connection, slugs: &[String]) -> Result<Subgraph> {
    let mut nodes = Vec::with_capacity(slugs.len());
    for s in slugs {
        if let Some(n) = get_node(conn, s)? {
            nodes.push(n);
        }
    }
    nodes.sort_by(|a, b| a.slug.cmp(&b.slug));
    let edges = fetch_edges_within(conn, slugs)?;
    Ok(Subgraph {
        nodes,
        edges,
        truncated: false,
    })
}

/// Body content read back from a single legal node. Layout mirrors the
/// frontmatter we write in ingest, so the agent can quote accurately without
/// needing to retrieve the surrounding graph first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArticleContent {
    pub slug: String,
    pub kind: NodeKind,
    pub label: String,
    /// Full vault_documents.content (statute: law + header + body; case: full md).
    pub content: String,
    /// Structured metadata (law_name, article_num, case_number, verdict_date, …).
    pub metadata: HashMap<String, String>,
    /// Sections parsed from case markdown (`판시사항`, `판결요지`, `참조조문`,
    /// `참조판례`, `판례내용`). Empty for statutes.
    pub sections: HashMap<String, String>,
}

pub fn read_article(conn: &Connection, slug: &str) -> Result<Option<ArticleContent>> {
    let Some((schema, id, doc_type)) = lookup_slug_in_legal(conn, slug)? else {
        return Ok(None);
    };
    let content: String = conn
        .query_row(
            &format!(
                "SELECT content FROM {schema}.vault_documents WHERE id = ?1"
            ),
            params![id],
            |r| r.get::<_, String>(0),
        )
        .with_context(|| format!("loading content for {slug}"))?;
    let kind = NodeKind::from_doc_type(&doc_type).unwrap();
    let metadata = load_frontmatter(conn, schema, id)?;
    let label = build_label(slug, kind, &metadata);
    let sections = match kind {
        NodeKind::Case => parse_case_sections(&content),
        NodeKind::Statute => HashMap::new(),
    };
    Ok(Some(ArticleContent {
        slug: slug.to_string(),
        kind,
        label,
        content,
        metadata,
        sections,
    }))
}

/// Lightweight hit from [`find_nodes`] — carries just enough to disambiguate
/// before calling [`read_article`] or [`neighbors`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindHit {
    pub slug: String,
    pub kind: NodeKind,
    pub label: String,
    /// Why this hit matched (`"exact-slug"` / `"exact-alias"` / `"parsed-citation"` /
    /// `"parsed-case-number"` / `"fts-fallback"`). Helps the caller weigh
    /// confidence.
    pub matched_via: &'static str,
}

/// Find legal nodes matching a human-readable query like:
///   - `statute::민법::839-2` (canonical slug)
///   - `민법 제839조의2` (natural-language citation)
///   - `민법 제839조의2(재산분할청구권)` (citation w/ parenthetical subtitle)
///   - `2024노3424` (bare case number)
///   - `대법원 2024노3424` (court-qualified alias)
///
/// Strategy (cheap → expensive):
///   1. Exact `vault_documents.title` match.
///   2. Exact `vault_aliases.alias` match.
///   3. Parse as statute citation → construct slug → exact lookup.
///   4. Parse as case number → construct slug → exact lookup.
///   5. FTS5 fallback on `vault_docs_fts` (trigram) — top-`limit` hits,
///      restricted to legal `doc_type`.
///
/// Returns up to `limit` hits with provenance tags for disambiguation.
pub fn find_nodes(conn: &Connection, query: &str, limit: usize) -> Result<Vec<FindHit>> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(vec![]);
    }
    let limit = limit.clamp(1, 20);
    let mut out: Vec<FindHit> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // 1. Exact slug — 100% confidence; short-circuit so the FTS fallback
    //    doesn't pollute the result with low-signal neighbours.
    if let Some(n) = get_node(conn, q)? {
        push_hit(&mut out, &mut seen, &n, "exact-slug");
        return Ok(out);
    }

    // 2. Exact alias — same 100% confidence; short-circuit.
    //    Domain-priority: search domain first, then main, so a domain
    //    alias shadowing a main alias resolves to the domain row.
    let mut alias_hit: Option<String> = None;
    for schema in active_schemas(conn) {
        let hit: Option<String> = conn
            .query_row(
                &format!(
                    "SELECT d.title FROM {schema}.vault_aliases va
                       JOIN {schema}.vault_documents d ON d.id = va.doc_id
                      WHERE va.alias = ?1
                        AND d.doc_type IN
                            ('statute_article','statute_supplement','case')"
                ),
                params![q],
                |r| r.get::<_, String>(0),
            )
            .ok();
        if hit.is_some() {
            alias_hit = hit;
            break;
        }
    }
    if let Some(slug) = alias_hit {
        if let Some(n) = get_node(conn, &slug)? {
            push_hit(&mut out, &mut seen, &n, "exact-alias");
            return Ok(out);
        }
    }

    // 3. Parse as statute citation (using the same regex as body-scanning).
    if out.len() < limit {
        let parsed = super::citation_patterns::extract_statute_citations(q, None);
        for pr in parsed {
            let slug = super::slug::statute_slug(&pr.law_name, pr.article, pr.article_sub);
            if let Some(n) = get_node(conn, &slug)? {
                push_hit(&mut out, &mut seen, &n, "parsed-citation");
                if out.len() >= limit {
                    break;
                }
            }
        }
    }

    // 4. Parse as bare case number.
    if out.len() < limit {
        let cases = super::citation_patterns::extract_case_numbers(q);
        for cr in cases {
            let slug = super::slug::case_slug(&cr.case_number);
            if let Some(n) = get_node(conn, &slug)? {
                push_hit(&mut out, &mut seen, &n, "parsed-case-number");
                if out.len() >= limit {
                    break;
                }
            }
        }
    }

    // 5. FTS5 fallback. Trigram tokenizer requires ≥3-char substrings; the
    //    vault_docs_fts view is over title+content so hits for e.g.
    //    `재산분할` will surface 제839조의2.
    //
    //    SQLite limitation: the FTS5 `MATCH` operator's LHS must be the
    //    unqualified FTS5 table name — schema-qualified (`main.vault_docs_fts MATCH …`)
    //    and aliased forms are rejected as "no such column". This means
    //    we can only run FTS against `main.vault_docs_fts` from this
    //    connection. For the domain schema we still get cross-schema
    //    coverage via the slug/alias paths (steps 2–4), which `legal_graph_find`
    //    callers rely on; substring/concept search across the bulk
    //    domain corpus is intentionally out of scope for this entry
    //    point and is served by the dedicated legal tools that open
    //    domain.db on a separate connection when needed.
    if out.len() < limit && q.chars().count() >= 3 {
        let fts_query = fts_escape(q);
        if !fts_query.is_empty() {
            let remaining = limit - out.len();
            let mut stmt = conn.prepare(
                "SELECT d.title
                   FROM vault_docs_fts f
                   JOIN vault_documents d ON d.id = f.rowid
                  WHERE vault_docs_fts MATCH ?1
                    AND d.doc_type IN
                        ('statute_article','statute_supplement','case')
                  ORDER BY bm25(vault_docs_fts)
                  LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![fts_query, remaining as i64], |r| {
                r.get::<_, String>(0)
            })?;
            for slug in rows.filter_map(Result::ok) {
                if seen.contains(&slug) {
                    continue;
                }
                if let Some(n) = get_node(conn, &slug)? {
                    push_hit(&mut out, &mut seen, &n, "fts-fallback");
                    if out.len() >= limit {
                        break;
                    }
                }
            }
        }
    }

    Ok(out)
}

// ───────── Internals ─────────

fn push_hit(
    out: &mut Vec<FindHit>,
    seen: &mut HashSet<String>,
    node: &Node,
    matched_via: &'static str,
) {
    if seen.insert(node.slug.clone()) {
        out.push(FindHit {
            slug: node.slug.clone(),
            kind: node.kind,
            label: node.label.clone(),
            matched_via,
        });
    }
}

/// Minimal FTS5 escape: strip punctuation that breaks the query parser,
/// leaving a whitespace-separated phrase. Empty if nothing useful remains.
fn fts_escape(q: &str) -> String {
    // Replace FTS operator characters with space; collapse whitespace.
    let cleaned: String = q
        .chars()
        .map(|c| match c {
            '"' | '\'' | '(' | ')' | ':' | '*' | '-' | '+' | '^' => ' ',
            other => other,
        })
        .collect();
    let parts: Vec<&str> = cleaned.split_whitespace().filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return String::new();
    }
    // Quote each term so trigram tokenizer matches substrings across our
    // ingested Korean content.
    parts
        .iter()
        .map(|p| format!("\"{}\"", p.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_case_sections(md: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut key: Option<String> = None;
    let mut val = String::new();
    for line in md.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            if let Some(k) = key.take() {
                out.insert(k, std::mem::take(&mut val).trim().to_string());
            }
            key = Some(rest.trim().to_string());
        } else if key.is_some() {
            val.push_str(line);
            val.push('\n');
        }
    }
    if let Some(k) = key {
        out.insert(k, val.trim().to_string());
    }
    out
}

// ───────── Graph traversal internals ─────────

fn pass_kinds(filter: &[NodeKind], kind: NodeKind) -> bool {
    filter.is_empty() || filter.contains(&kind)
}

/// All 1-hop neighbor slugs of `slug` — following OUTBOUND edges
/// (vault_links.source_doc_id = slug's id) and INBOUND edges (target_raw = slug
/// for unresolved links, or target_doc_id = slug's id for resolved ones).
///
/// Cross-schema: enumerates outbound + inbound edges from BOTH `main`
/// and `domain` (when attached). Inbound edges from a different schema
/// can only match by `target_raw == slug`, since cross-schema
/// `target_doc_id` is never written (per slug-only resolution policy).
fn fetch_direct_neighbors(conn: &Connection, slug: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();

    for schema in active_schemas(conn) {
        // Per-schema id lookup. The same slug may resolve in multiple
        // schemas; we walk each schema's local edges independently.
        let id: Option<i64> = conn
            .query_row(
                &format!(
                    "SELECT id FROM {schema}.vault_documents WHERE title = ?1"
                ),
                params![slug],
                |r| r.get::<_, i64>(0),
            )
            .ok();

        if let Some(id) = id {
            // Outbound: legal_node → whatever. target_raw always set.
            {
                let mut stmt = conn.prepare(&format!(
                    "SELECT target_raw FROM {schema}.vault_links WHERE source_doc_id = ?1"
                ))?;
                let rows = stmt.query_map(params![id], |r| r.get::<_, String>(0))?;
                for r in rows {
                    out.push(r?);
                }
            }
            // Inbound (intra-schema): use target_doc_id when resolved,
            // target_raw otherwise.
            {
                let mut stmt = conn.prepare(&format!(
                    "SELECT d.title
                       FROM {schema}.vault_links vl
                       JOIN {schema}.vault_documents d ON d.id = vl.source_doc_id
                      WHERE (vl.target_doc_id = ?1 AND vl.is_resolved = 1)
                         OR (vl.target_raw = ?2 AND vl.is_resolved = 0)"
                ))?;
                let rows = stmt.query_map(params![id, slug], |r| r.get::<_, String>(0))?;
                for r in rows {
                    out.push(r?);
                }
            }
        }

        // Inbound (cross-schema-safe): unresolved links whose
        // `target_raw == slug`, regardless of whether the slug resolves
        // in this schema. Catches `main.vault_links → domain.slug`.
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT d.title
                   FROM {schema}.vault_links vl
                   JOIN {schema}.vault_documents d ON d.id = vl.source_doc_id
                  WHERE vl.target_raw = ?1 AND vl.is_resolved = 0"
            ))?;
            let rows = stmt.query_map(params![slug], |r| r.get::<_, String>(0))?;
            for r in rows {
                out.push(r?);
            }
        }
    }

    // Restrict to legal nodes only (other vault docs may share brain.db),
    // checking each schema and keeping any slug that surfaces in either.
    if out.is_empty() {
        return Ok(out);
    }
    let placeholders = std::iter::repeat("?")
        .take(out.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut keep: HashSet<String> = HashSet::new();
    for schema in active_schemas(conn) {
        let q = format!(
            "SELECT title FROM {schema}.vault_documents
               WHERE title IN ({placeholders})
                 AND doc_type IN ('statute_article','statute_supplement','case')"
        );
        let mut stmt = conn.prepare(&q)?;
        let params_dyn: Vec<&dyn rusqlite::ToSql> =
            out.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(params_dyn.as_slice(), |r| r.get::<_, String>(0))?;
        for r in rows.filter_map(Result::ok) {
            keep.insert(r);
        }
    }
    out.retain(|s| keep.contains(s));
    out.sort();
    out.dedup();
    Ok(out)
}

/// All edges whose both endpoints are in `slugs`.
///
/// Cross-schema: collects edges from BOTH `main` and `domain` (when
/// attached). Edges are matched purely by slug (`src.title` +
/// `vl.target_raw`) so cross-schema references resolve naturally —
/// `main.vault_links` whose `target_raw` names a `domain.vault_documents`
/// slug are kept iff that slug is in the input set.
fn fetch_edges_within(conn: &Connection, slugs: &[String]) -> Result<Vec<Edge>> {
    if slugs.is_empty() {
        return Ok(vec![]);
    }
    let placeholders = std::iter::repeat("?")
        .take(slugs.len())
        .collect::<Vec<_>>()
        .join(",");

    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    for schema in active_schemas(conn) {
        let q = format!(
            "SELECT src.title, vl.target_raw, vl.display_text, vl.context, vl.is_resolved
               FROM {schema}.vault_links vl
               JOIN {schema}.vault_documents src ON src.id = vl.source_doc_id
              WHERE src.title IN ({ph})
                AND vl.target_raw IN ({ph})
                AND src.doc_type IN ('statute_article','statute_supplement','case')",
            ph = placeholders
        );
        let mut stmt = conn.prepare(&q)?;
        let params_dyn: Vec<&dyn rusqlite::ToSql> = slugs
            .iter()
            .chain(slugs.iter())
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();
        let rows = stmt.query_map(params_dyn.as_slice(), |r| {
            let evidence: Option<String> = r.get(3)?;
            // `[pre:YYYYMMDD] …` prefix carries the 구법 applicable-law
            // signal. Strip it from `evidence` (cleaner display) and
            // surface as a structured field.
            let (revision_cutoff, evidence) = split_pre_marker(evidence);
            Ok(Edge {
                source_slug: r.get(0)?,
                target_slug: r.get(1)?,
                relation: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                evidence,
                resolved: r.get::<_, i64>(4)? != 0,
                revision_cutoff,
            })
        })?;
        for e in rows.filter_map(Result::ok) {
            let key = (
                e.source_slug.clone(),
                e.target_slug.clone(),
                e.relation.clone(),
            );
            if seen.insert(key) {
                edges.push(e);
            }
        }
    }
    edges.sort_by(|a, b| {
        (
            a.source_slug.as_str(),
            a.target_slug.as_str(),
            a.relation.as_str(),
        )
            .cmp(&(
                b.source_slug.as_str(),
                b.target_slug.as_str(),
                b.relation.as_str(),
            ))
    });
    Ok(edges)
}

/// Applicable-version decision returned by [`pick_applicable_version`].
/// Encodes the 제1원칙 / Tier 1 branch the decision took + the picked
/// supplement slug + decision metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicableVersion {
    /// `"primary_verdict_date"` — plain citation, applied the law in
    /// force at `verdict_date` (제1원칙).
    /// `"revision_cutoff"` — citation was `구 {법}(... 개정되기 전의
    /// 것)`, applied the version effective immediately BEFORE the
    /// cutoff date.
    /// `"fallback_latest"` — neither signal was usable (missing
    /// verdict_date AND no cutoff); picked the newest ingested
    /// supplement as a last resort.
    /// `"none"` — no supplement nodes exist for this statute at all.
    pub branch: String,
    /// YYYYMMDD the decision was anchored to (verdict_date or cutoff).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_date: Option<String>,
    /// The chosen supplement node's slug (if one was picked).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supplement_slug: Option<String>,
    /// The chosen supplement's effective_date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_date: Option<String>,
    /// The chosen supplement's promulgation number (법률 제N호).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promulgation_no: Option<String>,
    /// The promulgation date. Used by callers to distinguish adjacent
    /// versions when the effective_date ties.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promulgation_date: Option<String>,
    /// Versioned article slug pointing at the exact snapshot of the
    /// article body that was in force. Constructed as
    /// `statute::{법}::{N}@{promulgation_date}`. Set when such a
    /// versioned node exists in brain.db; agents should call
    /// `legal_read_article` with this slug to retrieve the historical
    /// body text. `None` means the picker settled on a version whose
    /// body isn't archived — either because only current ingestion
    /// ran or the specific version snapshot wasn't in the corpus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub versioned_article_slug: Option<String>,
    /// A short human-readable explanation of the decision.
    pub explanation: String,
}

/// Given a `case` slug, the `statute_article` slug it cites, and the
/// `revision_cutoff` for that specific edge (from `Edge.revision_cutoff`
/// or extracted fresh), pick the applicable statute version using the
/// 2-tier rule:
///
///   1. If `edge_cutoff` is `Some(YYYYMMDD)` → pick the supplement
///      with the greatest `effective_date ≤ cutoff − 1 day`.
///   2. Else → pick the supplement with the greatest
///      `effective_date ≤ case.verdict_date`.
///
/// "Supplement with the greatest" is an index into `vault_documents`
/// rows `doc_type='statute_supplement'` + `parent_law` frontmatter =
/// this statute's law_name.
///
/// Returns `ApplicableVersion::branch = "none"` if the statute has no
/// supplement nodes, or `"fallback_latest"` if both the cutoff and
/// verdict_date are absent.
pub fn pick_applicable_version(
    conn: &Connection,
    case_slug: &str,
    statute_slug: &str,
    edge_cutoff: Option<&str>,
) -> Result<ApplicableVersion> {
    // Work out which law this statute article belongs to — we look up
    // the article's `law_name` frontmatter so we can list its
    // supplements. Domain-priority: try domain first, then main.
    let article_lookup: Option<(&'static str, i64)> = active_schemas(conn)
        .iter()
        .find_map(|schema| {
            conn.query_row(
                &format!(
                    "SELECT id FROM {schema}.vault_documents WHERE title = ?1
                       AND doc_type IN ('statute_article','statute_supplement')"
                ),
                params![statute_slug],
                |r| r.get::<_, i64>(0),
            )
            .ok()
            .map(|id| (*schema, id))
        });
    let Some((article_schema, aid)) = article_lookup else {
        return Ok(ApplicableVersion {
            branch: "none".to_string(),
            anchor_date: None,
            supplement_slug: None,
            effective_date: None,
            promulgation_no: None,
            promulgation_date: None,
            versioned_article_slug: None,
            explanation: format!("statute article not found: {statute_slug}"),
        });
    };
    let fm = load_frontmatter(conn, article_schema, aid)?;
    let law_name = match fm.get("law_name").or_else(|| fm.get("parent_law")) {
        Some(n) => n.clone(),
        None => {
            return Ok(ApplicableVersion {
                branch: "none".to_string(),
                anchor_date: None,
                supplement_slug: None,
                effective_date: None,
                promulgation_no: None,
                promulgation_date: None,
                versioned_article_slug: None,
                explanation: format!("no law_name on {statute_slug}"),
            });
        }
    };

    // Fetch the case's verdict_date (Tier 2 anchor). Search across
    // schemas — cases may live in main (user-pasted) or domain (shipped
    // corpus). First non-empty hit wins.
    let mut verdict_date: Option<String> = None;
    for schema in active_schemas(conn) {
        let v: Option<String> = conn
            .query_row(
                &format!(
                    "SELECT vf.value FROM {schema}.vault_frontmatter vf
                       JOIN {schema}.vault_documents d ON d.id = vf.doc_id
                      WHERE d.title = ?1 AND vf.key = 'verdict_date'"
                ),
                params![case_slug],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten();
        if v.is_some() {
            verdict_date = v;
            break;
        }
    }

    // Work out the anchor date + branch.
    let (branch, anchor_date_owned) = match edge_cutoff {
        Some(c) if c.len() == 8 => {
            // Cutoff is exclusive — we want the last supplement effective
            // BEFORE this date. Subtract one day.
            let prev = previous_day_yyyymmdd(c).unwrap_or_else(|| c.to_string());
            ("revision_cutoff", Some(prev))
        }
        _ => match verdict_date.as_ref() {
            Some(vd) if vd.len() == 8 => ("primary_verdict_date", Some(vd.clone())),
            _ => ("fallback_latest", None),
        },
    };

    // Query supplements of this law, ordered newest-first by
    // effective_date. For fallback_latest we just take the top row.
    let supplements = list_supplements_of_law(conn, &law_name)?;

    let picked = match &anchor_date_owned {
        Some(anchor) => supplements
            .iter()
            .filter(|s| s.effective_date.as_deref().is_some_and(|d| d <= anchor.as_str()))
            .max_by(|a, b| a.effective_date.cmp(&b.effective_date))
            .cloned(),
        None => supplements.into_iter().next(),
    };

    // Extract article_num / article_sub from the statute slug so we can
    // build a versioned-article slug pointing at the historical body.
    // Slug form: `statute::{법}::{N}[-M]`; bail silently if malformed.
    let (article_num, article_sub) = parse_article_nums_from_slug(statute_slug);

    match picked {
        Some(s) => {
            // Compose the versioned article slug from the chosen
            // supplement's promulgation_date + article numbers, then
            // verify the versioned node actually exists before
            // surfacing it — we don't fabricate slugs that can't be
            // read.
            let versioned_slug = match (article_num, s.promulgation_date.as_deref()) {
                (Some(n), Some(pd)) if pd.len() == 8 => {
                    let candidate =
                        super::slug::versioned_statute_slug(&law_name, n, article_sub, pd);
                    // Check both schemas — versioned snapshots may live
                    // in either the user's brain.db or the shipped
                    // domain corpus.
                    let exists = active_schemas(conn).iter().any(|schema| {
                        conn.query_row(
                            &format!(
                                "SELECT 1 FROM {schema}.vault_documents
                                  WHERE title = ?1
                                    AND doc_type = 'statute_article_version'"
                            ),
                            params![candidate],
                            |_| Ok(true),
                        )
                        .unwrap_or(false)
                    });
                    if exists { Some(candidate) } else { None }
                }
                _ => None,
            };

            Ok(ApplicableVersion {
                branch: branch.to_string(),
                anchor_date: anchor_date_owned.clone(),
                supplement_slug: Some(s.slug),
                effective_date: s.effective_date,
                promulgation_no: s.promulgation_no,
                promulgation_date: s.promulgation_date,
                versioned_article_slug: versioned_slug,
                explanation: match branch {
                    "revision_cutoff" => format!(
                        "구법 citation: applied version whose effective_date ≤ {}",
                        anchor_date_owned.as_deref().unwrap_or("?")
                    ),
                    "primary_verdict_date" => format!(
                        "제1원칙: plain citation, applied version in force at verdict_date {}",
                        anchor_date_owned.as_deref().unwrap_or("?")
                    ),
                    _ => "fallback: no verdict_date, used newest supplement".to_string(),
                },
            })
        }
        None => Ok(ApplicableVersion {
            branch: "none".to_string(),
            anchor_date: anchor_date_owned,
            supplement_slug: None,
            effective_date: None,
            promulgation_no: None,
            promulgation_date: None,
            versioned_article_slug: None,
            explanation: format!(
                "no supplement for law `{law_name}` matches the anchor date"
            ),
        }),
    }
}

fn parse_article_nums_from_slug(slug: &str) -> (Option<u32>, Option<u32>) {
    let prefix = "statute::";
    let rest = match slug.strip_prefix(prefix) {
        Some(r) => r,
        None => return (None, None),
    };
    let mut parts = rest.split("::");
    let _law = parts.next();
    let art_part = match parts.next() {
        Some(a) => a,
        None => return (None, None),
    };
    // `43-2` or `43`
    let mut split = art_part.split('-');
    let n: Option<u32> = split.next().and_then(|s| s.parse().ok());
    let sub: Option<u32> = split.next().and_then(|s| s.parse().ok());
    (n, sub)
}

/// Minimal shape returned by [`list_supplements_of_law`].
#[derive(Debug, Clone)]
struct SupplementRow {
    slug: String,
    effective_date: Option<String>,
    promulgation_no: Option<String>,
    promulgation_date: Option<String>,
}

fn list_supplements_of_law(conn: &Connection, law_name: &str) -> Result<Vec<SupplementRow>> {
    // Collect from each active schema then merge. Domain-priority on
    // slug conflicts: when the same supplement slug surfaces in both
    // schemas, the domain copy wins (we iterate domain first).
    let mut by_slug: HashMap<String, SupplementRow> = HashMap::new();
    for schema in active_schemas(conn) {
        let mut stmt = conn.prepare(&format!(
            "SELECT d.title,
                    MAX(CASE WHEN vf.key='effective_date'    THEN vf.value END) AS eff,
                    MAX(CASE WHEN vf.key='promulgation_no'   THEN vf.value END) AS pno,
                    MAX(CASE WHEN vf.key='promulgation_date' THEN vf.value END) AS pdate,
                    MAX(CASE WHEN vf.key='parent_law'        THEN vf.value END) AS parent
               FROM {schema}.vault_documents d
               JOIN {schema}.vault_frontmatter vf ON vf.doc_id = d.id
              WHERE d.doc_type = 'statute_supplement'
              GROUP BY d.id, d.title
             HAVING parent = ?1"
        ))?;
        let rows = stmt.query_map(params![law_name], |r| {
            Ok(SupplementRow {
                slug: r.get(0)?,
                effective_date: r.get::<_, Option<String>>(1)?,
                promulgation_no: r.get::<_, Option<String>>(2)?,
                promulgation_date: r.get::<_, Option<String>>(3)?,
            })
        })?;
        for sup in rows.filter_map(Result::ok) {
            by_slug.entry(sup.slug.clone()).or_insert(sup);
        }
    }
    let mut out: Vec<SupplementRow> = by_slug.into_values().collect();
    // Sort effective_date DESC, NULLs last.
    out.sort_by(|a, b| match (&a.effective_date, &b.effective_date) {
        (Some(x), Some(y)) => y.cmp(x),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.slug.cmp(&b.slug),
    });
    Ok(out)
}

fn previous_day_yyyymmdd(s: &str) -> Option<String> {
    use chrono::NaiveDate;
    if s.len() != 8 {
        return None;
    }
    let y: i32 = s[..4].parse().ok()?;
    let m: u32 = s[4..6].parse().ok()?;
    let d: u32 = s[6..8].parse().ok()?;
    let date = NaiveDate::from_ymd_opt(y, m, d)?;
    let prev = date.pred_opt()?;
    Some(format!(
        "{:04}{:02}{:02}",
        prev.year_naive(),
        prev.month_naive(),
        prev.day_naive()
    ))
}

/// Helper to keep the `Datelike` trait method calls inline-friendly.
trait NaiveDateExt {
    fn year_naive(&self) -> i32;
    fn month_naive(&self) -> u32;
    fn day_naive(&self) -> u32;
}
impl NaiveDateExt for chrono::NaiveDate {
    fn year_naive(&self) -> i32 {
        <Self as chrono::Datelike>::year(self)
    }
    fn month_naive(&self) -> u32 {
        <Self as chrono::Datelike>::month(self)
    }
    fn day_naive(&self) -> u32 {
        <Self as chrono::Datelike>::day(self)
    }
}

/// If `evidence` starts with `[pre:YYYYMMDD] `, split into
/// `(Some(YYYYMMDD), remainder)`. Otherwise `(None, original)`.
fn split_pre_marker(evidence: Option<String>) -> (Option<String>, Option<String>) {
    let Some(s) = evidence else {
        return (None, None);
    };
    if let Some(rest) = s.strip_prefix("[pre:") {
        if let Some(end) = rest.find("] ") {
            let date = &rest[..end];
            if date.len() == 8 && date.chars().all(|c| c.is_ascii_digit()) {
                return (Some(date.to_string()), Some(rest[end + 2..].to_string()));
            }
        }
    }
    (None, Some(s))
}

fn hydrate_node(
    conn: &Connection,
    schema: &str,
    id: i64,
    slug: &str,
    kind: NodeKind,
) -> Result<Node> {
    let fm = load_frontmatter(conn, schema, id)?;
    let label = build_label(slug, kind, &fm);
    Ok(Node {
        id,
        slug: slug.to_string(),
        kind,
        label,
        law_name: fm.get("law_name").cloned(),
        article_key: fm.get("article_key").cloned(),
        article_title_kw: fm.get("article_title_kw").cloned(),
        case_number: fm.get("case_number").cloned(),
        court_name: fm.get("court_name").cloned(),
        verdict_date: fm.get("verdict_date").cloned(),
        case_name: fm.get("case_name").cloned(),
        effective_date: fm.get("effective_date").cloned(),
        promulgation_date: fm.get("promulgation_date").cloned(),
        latest_amendment_date: fm.get("latest_amendment_date").cloned(),
        incident_date_earliest: fm.get("incident_date_earliest").cloned(),
        incident_date_latest: fm.get("incident_date_latest").cloned(),
    })
}

fn load_frontmatter(
    conn: &Connection,
    schema: &str,
    doc_id: i64,
) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT key, value FROM {schema}.vault_frontmatter WHERE doc_id = ?1"
    ))?;
    let rows = stmt.query_map(params![doc_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default()))
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

fn build_label(slug: &str, kind: NodeKind, fm: &HashMap<String, String>) -> String {
    match kind {
        NodeKind::Statute => {
            // Supplement? — use its parent_law + pretty promulgation date.
            if fm.get("kind").map(String::as_str) == Some("supplement") {
                let law = fm.get("parent_law").cloned().unwrap_or_default();
                let anc = fm.get("promulgation_no").cloned().unwrap_or_default();
                let date = fm
                    .get("promulgation_date")
                    .filter(|d| d.len() == 8)
                    .map(|d| format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..8]))
                    .unwrap_or_default();
                return match (law.is_empty(), anc.is_empty(), date.is_empty()) {
                    (false, false, false) => format!("{law} 부칙 (법률 제{anc}호, {date})"),
                    (false, false, true) => format!("{law} 부칙 (법률 제{anc}호)"),
                    _ => slug.to_string(),
                };
            }
            let law = fm.get("law_name").cloned().unwrap_or_default();
            let header = fm.get("article_header").cloned();
            if let Some(h) = header {
                if !law.is_empty() {
                    return format!("{law} {h}");
                }
                return h;
            }
            slug.to_string()
        }
        NodeKind::Case => {
            let case_no = fm.get("case_number").cloned().unwrap_or_default();
            let name = fm.get("case_name").cloned();
            match name {
                Some(n) if !n.is_empty() && !case_no.is_empty() => format!("{case_no} · {n}"),
                _ => case_no,
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// 쟁점 분석 (Issue Analysis) — 적용법조 그래프 기반 핵심 쟁점 식별
// ═══════════════════════════════════════════════════════════════════
//
// 변호사의 판례 검색 워크플로를 코드로 구현:
//
//   1. 키워드 → 판례 검색
//   2. 판례의 적용법조를 노드로 삼아 관련 판례 그래프를 확장
//   3. 그래프에서 연결 밀도(degree centrality)가 높은 법조 = 핵심 쟁점 적용법조
//   4. 연결 밀도가 낮은 법조 = 부수적 쟁점 (추가 공격방어방법 탐색 후보)
//   5. 연결 노드가 집중되는 판례 = 리딩 판결
//
// "범용 법조" 필터: 형법 제37조(경합범) 등 거의 모든 사건에 형식적으로
// 적용되는 조항은 높은 degree에도 불구하고 쟁점 가치가 낮다.
// `BOILERPLATE_STATUTES` 목록으로 제외하거나 감점한다.

/// Statutes that are procedurally applied to a large fraction of cases
/// but rarely constitute the substantive legal issue. These are
/// penalised (not removed) in centrality scoring so they don't crowd
/// out actual issue statutes.
///
/// Maintained as a const list. If the list grows, migrate to a DB
/// tag (`tag_type = 'boilerplate'`) for easier maintenance.
pub const BOILERPLATE_STATUTES: &[&str] = &[
    // 형법 — 경합범/누범/작량감경/집행유예 등
    "statute::형법::37",   // 경합범
    "statute::형법::38",   // 경합범 처리
    "statute::형법::39",   // 판결을 받지 않은 경합범 처리
    "statute::형법::40",   // 상상적경합
    "statute::형법::35",   // 누범
    "statute::형법::53",   // 작량감경
    "statute::형법::55",   // 법률상감경
    "statute::형법::62",   // 집행유예
    "statute::형법::57",   // 판결선고전 구금일수 산입
    "statute::형법::70",   // 노역장유치
    // 형사소송법 — 절차 조항
    "statute::형사소송법::334",  // 구속기간
    "statute::형사소송법::369",  // 항소
    "statute::형사소송법::383",  // 상고이유
    "statute::형사소송법::396",  // 파기자판
    // 민사소송법 — 절차 조항
    "statute::민사소송법::288",  // 증명 불필요 사실
    "statute::민사소송법::217",  // 자유심증주의
    // 소송촉진등에관한특례법
    "statute::소송촉진등에관한특례법::3",  // 지연이자
];

/// A statute node with its degree centrality in the issue graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueStatute {
    /// Canonical slug, e.g. `statute::민법::750`.
    pub slug: String,
    /// Human label, e.g. `민법 제750조(불법행위의 내용)`.
    pub label: String,
    pub law_name: String,
    pub article_key: String,
    /// Raw count of distinct cases citing this statute within the analysis scope.
    pub citing_case_count: usize,
    /// Centrality score (0.0–1.0). Higher = more central = more likely core issue.
    /// Boilerplate statutes are penalised by halving their raw score.
    pub centrality: f64,
    /// Classification derived from centrality ranking.
    pub issue_tier: IssueTier,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueTier {
    /// Core issue — highest centrality, most cases converge on this statute.
    Core,
    /// Supporting issue — moderate centrality, relevant but not central.
    Supporting,
    /// Peripheral — low centrality, potential supplementary attack/defence angle.
    Peripheral,
    /// Boilerplate — procedural/formal statute, not a substantive issue.
    Boilerplate,
}

/// A case node ranked by how many issue-statutes it connects to.
/// Cases that cite many core-issue statutes are leading-case candidates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankedCase {
    pub slug: String,
    pub case_number: String,
    pub case_name: Option<String>,
    pub court_name: Option<String>,
    pub verdict_date: Option<String>,
    /// Number of core + supporting issue statutes this case cites.
    pub issue_statute_count: usize,
    /// Sum of centrality scores of cited statutes → proxy for "leading-ness".
    pub leading_score: f64,
}

/// Full issue-analysis result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueAnalysis {
    /// Statutes ranked by centrality (core → supporting → peripheral → boilerplate).
    pub statutes: Vec<IssueStatute>,
    /// Cases ranked by leading_score (highest = most likely leading case).
    pub cases: Vec<RankedCase>,
    /// Total cases in the analysis scope.
    pub total_cases: usize,
    /// Total distinct statutes found.
    pub total_statutes: usize,
}

/// Analyse the issue-statute graph starting from a set of seed cases.
///
/// This implements the 변호사 워크플로:
///   1. From seed cases, collect all cited statutes (참조조문 edges).
///   2. For each statute, find all cases that cite it (reverse lookup).
///   3. For the expanded case set, collect their statutes again (2-hop).
///   4. Score statutes by degree centrality (citing_case_count / total_cases).
///   5. Classify into core / supporting / peripheral / boilerplate tiers.
///   6. Score cases by sum of centrality of their cited statutes → leading cases.
///
/// `max_hops`: number of statute→case→statute expansion rounds (1 = direct, 2 = one expansion).
/// `max_cases`: cap on total cases to prevent runaway on broad statutes.
pub fn issue_analysis(
    conn: &Connection,
    seed_case_slugs: &[String],
    max_hops: usize,
    max_cases: usize,
) -> Result<IssueAnalysis> {
    let max_hops = max_hops.min(3); // safety cap
    let max_cases = max_cases.min(5000);

    // ── Phase 1: Expand the case↔statute graph ──
    // statute_slug → set of case_slugs that cite it
    let mut statute_to_cases: HashMap<String, HashSet<String>> = HashMap::new();
    // case_slug → set of statute_slugs it cites
    let mut case_to_statutes: HashMap<String, HashSet<String>> = HashMap::new();
    // Frontier of case slugs to expand
    let mut case_frontier: HashSet<String> = seed_case_slugs.iter().cloned().collect();
    let mut all_cases: HashSet<String> = case_frontier.clone();

    for _hop in 0..max_hops {
        // For each case in the frontier, find its statute citations.
        let mut new_statutes: HashSet<String> = HashSet::new();
        for case_slug in &case_frontier {
            let statutes = case_cited_statutes(conn, case_slug)?;
            for s in &statutes {
                statute_to_cases
                    .entry(s.clone())
                    .or_default()
                    .insert(case_slug.clone());
                new_statutes.insert(s.clone());
            }
            case_to_statutes
                .entry(case_slug.clone())
                .or_default()
                .extend(statutes);
        }

        // For each newly discovered statute, find all citing cases.
        let mut next_frontier: HashSet<String> = HashSet::new();
        for statute_slug in &new_statutes {
            let citing = statute_citing_cases(conn, statute_slug, max_cases)?;
            for c in &citing {
                statute_to_cases
                    .entry(statute_slug.clone())
                    .or_default()
                    .insert(c.clone());
                case_to_statutes
                    .entry(c.clone())
                    .or_default()
                    .insert(statute_slug.clone());
                if !all_cases.contains(c) {
                    next_frontier.insert(c.clone());
                    all_cases.insert(c.clone());
                }
            }
            if all_cases.len() >= max_cases {
                break;
            }
        }

        case_frontier = next_frontier;
        if case_frontier.is_empty() || all_cases.len() >= max_cases {
            break;
        }
    }

    let total_cases = all_cases.len().max(1);
    let total_statutes = statute_to_cases.len();

    // ── Phase 2: Score statutes by degree centrality ──
    let is_boilerplate =
        |slug: &str| BOILERPLATE_STATUTES.iter().any(|b| slug == *b);

    let mut issue_statutes: Vec<IssueStatute> = Vec::with_capacity(total_statutes);
    for (slug, cases) in &statute_to_cases {
        let raw_centrality = cases.len() as f64 / total_cases as f64;
        let centrality = if is_boilerplate(slug) {
            raw_centrality * 0.1 // 90% penalty for boilerplate
        } else {
            raw_centrality
        };

        // Hydrate label from DB.
        let (label, law_name, article_key) = match get_node(conn, slug)? {
            Some(n) => (
                n.label,
                n.law_name.unwrap_or_default(),
                n.article_key.unwrap_or_default(),
            ),
            None => (slug.clone(), String::new(), String::new()),
        };

        issue_statutes.push(IssueStatute {
            slug: slug.clone(),
            label,
            law_name,
            article_key,
            citing_case_count: cases.len(),
            centrality,
            issue_tier: IssueTier::Peripheral, // placeholder, assigned below
        });
    }

    // Sort by centrality descending. NaN-safe: any NaN compares as
    // Equal, sinking it to the end of an otherwise-ordered list
    // rather than panicking.
    issue_statutes.sort_by(|a, b| {
        b.centrality
            .partial_cmp(&a.centrality)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // ── Phase 3: Classify tiers ──
    // Thresholds are relative to the highest non-boilerplate centrality.
    let max_centrality = issue_statutes
        .iter()
        .filter(|s| !is_boilerplate(&s.slug))
        .map(|s| s.centrality)
        .fold(0.0_f64, f64::max);

    for s in &mut issue_statutes {
        if is_boilerplate(&s.slug) {
            s.issue_tier = IssueTier::Boilerplate;
        } else if max_centrality > 0.0 {
            let ratio = s.centrality / max_centrality;
            if ratio >= 0.5 {
                s.issue_tier = IssueTier::Core;
            } else if ratio >= 0.15 {
                s.issue_tier = IssueTier::Supporting;
            } else {
                s.issue_tier = IssueTier::Peripheral;
            }
        }
    }

    // ── Phase 4: Score cases → leading case identification ──
    // A case's "leading score" = sum of centrality of all core+supporting
    // issue statutes it cites. Higher score → more issue-rich → more likely
    // to be the leading judgment.
    let centrality_map: HashMap<&str, f64> = issue_statutes
        .iter()
        .map(|s| (s.slug.as_str(), s.centrality))
        .collect();

    let mut ranked_cases: Vec<RankedCase> = Vec::with_capacity(all_cases.len());
    for case_slug in &all_cases {
        let statutes = case_to_statutes.get(case_slug.as_str());
        let (issue_count, leading_score) = statutes
            .map(|ss| {
                let mut count = 0usize;
                let mut score = 0.0f64;
                for s in ss {
                    if let Some(&c) = centrality_map.get(s.as_str()) {
                        // Only count non-boilerplate
                        if !is_boilerplate(s) {
                            count += 1;
                            score += c;
                        }
                    }
                }
                (count, score)
            })
            .unwrap_or((0, 0.0));

        // Hydrate case metadata.
        let (case_number, case_name, court_name, verdict_date) = match get_node(conn, case_slug)? {
            Some(n) => (
                n.case_number.unwrap_or_else(|| case_slug.clone()),
                n.case_name,
                n.court_name,
                n.verdict_date,
            ),
            None => (case_slug.clone(), None, None, None),
        };

        ranked_cases.push(RankedCase {
            slug: case_slug.clone(),
            case_number,
            case_name,
            court_name,
            verdict_date,
            issue_statute_count: issue_count,
            leading_score,
        });
    }

    // Sort by leading_score descending, then by verdict_date descending for ties.
    ranked_cases.sort_by(|a, b| {
        b.leading_score
            .partial_cmp(&a.leading_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.verdict_date
                    .as_deref()
                    .unwrap_or("")
                    .cmp(a.verdict_date.as_deref().unwrap_or(""))
            })
    });

    Ok(IssueAnalysis {
        statutes: issue_statutes,
        cases: ranked_cases,
        total_cases,
        total_statutes,
    })
}

// ── Helpers for issue_analysis ──

/// Find all statute slugs cited by a case (outbound `cites` edges).
fn case_cited_statutes(conn: &Connection, case_slug: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for schema in active_schemas(conn) {
        let mut stmt = conn.prepare(&format!(
            "SELECT l.target_raw FROM {schema}.vault_links l
             JOIN {schema}.vault_documents d ON d.id = l.source_doc_id
             WHERE d.title = ?1 AND l.display_text = 'cites'
               AND l.target_raw LIKE 'statute::%'"
        ))?;
        let rows = stmt.query_map(params![case_slug], |r| r.get::<_, String>(0))?;
        for row in rows {
            out.push(row?);
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Find all case slugs that cite a given statute (reverse lookup via vault_links).
fn statute_citing_cases(
    conn: &Connection,
    statute_slug: &str,
    max_results: usize,
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for schema in active_schemas(conn) {
        let mut stmt = conn.prepare(&format!(
            "SELECT d.title FROM {schema}.vault_links l
             JOIN {schema}.vault_documents d ON d.id = l.source_doc_id
             WHERE l.target_raw = ?1 AND l.display_text = 'cites'
               AND d.doc_type = 'case'
             LIMIT ?2"
        ))?;
        let remaining = max_results.saturating_sub(out.len());
        let rows = stmt.query_map(params![statute_slug, remaining as i64], |r| {
            r.get::<_, String>(0)
        })?;
        for row in rows {
            out.push(row?);
        }
        if out.len() >= max_results {
            break;
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::legal::{extract_case, extract_statute, ingest_case, ingest_statute};
    use crate::vault::schema::init_schema;
    use parking_lot::Mutex;
    use std::sync::Arc;

    const STATUTE_MD: &str = r#"# 근로기준법

```json
{
  "meta": {"lsNm": "근로기준법", "ancYd": "20251001", "efYd": "20251001"},
  "title": "근로기준법",
  "articles": [
    {"anchor": "J36:0", "number": "제36조(금품 청산)", "text": "제36조(금품 청산) 사용자는 14일 이내에 지급한다."},
    {"anchor": "J43:0", "number": "제43조(임금 지급)", "text": "제43조(임금 지급) ① 임금은 통화로 지급한다."},
    {"anchor": "J109:0", "number": "제109조(벌칙)", "text": "제109조 제36조, 제43조를 위반한 자는 처벌한다."}
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
대법원 2012. 9. 13. 선고 2012도3166 판결
## 판시사항
holding
## 판결요지
summary
## 판례내용
body
"#;

    fn setup() -> Arc<Mutex<Connection>> {
        let c = Connection::open_in_memory().unwrap();
        init_schema(&c).unwrap();
        let conn = Arc::new(Mutex::new(c));
        let sdoc = extract_statute(STATUTE_MD, "/x/20251001/근로기준법.md").unwrap();
        ingest_statute(&conn, &sdoc).unwrap();
        let cdoc = extract_case(CASE_MD, "/x/20250530/2024노3424_400102_형사_606941.md").unwrap();
        ingest_case(&conn, &cdoc).unwrap();
        conn
    }

    #[test]
    fn get_node_hydrates_statute_metadata() {
        let conn_a = setup();
        let g = conn_a.lock();
        let n = get_node(&g, "statute::근로기준법::109").unwrap().unwrap();
        assert_eq!(n.kind, NodeKind::Statute);
        assert_eq!(n.law_name.as_deref(), Some("근로기준법"));
        assert_eq!(n.article_key.as_deref(), Some("109"));
        assert!(n.label.contains("근로기준법"));
        assert!(n.label.contains("제109조"));
    }

    #[test]
    fn get_node_hydrates_case_metadata() {
        let conn_a = setup();
        let g = conn_a.lock();
        let n = get_node(&g, "case::2024노3424").unwrap().unwrap();
        assert_eq!(n.kind, NodeKind::Case);
        assert_eq!(n.case_number.as_deref(), Some("2024노3424"));
        assert_eq!(n.court_name.as_deref(), Some("수원지법"));
        assert_eq!(n.verdict_date.as_deref(), Some("20250530"));
    }

    #[test]
    fn neighbors_1hop_from_statute_reaches_cases_and_internal_refs() {
        let conn_a = setup();
        let g = conn_a.lock();
        let sg = neighbors(&g, "statute::근로기준법::36", 1, &[]).unwrap();
        let slugs: HashSet<_> = sg.nodes.iter().map(|n| n.slug.as_str()).collect();
        // The case cites 제36조 → should appear at depth 1.
        assert!(slugs.contains("case::2024노3424"));
        // 제109조 cites 제36조 → appears at depth 1 via inbound edge.
        assert!(slugs.contains("statute::근로기준법::109"));
    }

    #[test]
    fn neighbors_respects_kinds_filter() {
        let conn_a = setup();
        let g = conn_a.lock();
        let sg = neighbors(&g, "statute::근로기준법::36", 1, &[NodeKind::Statute]).unwrap();
        // Case nodes must be excluded.
        assert!(sg
            .nodes
            .iter()
            .all(|n| matches!(n.kind, NodeKind::Statute)));
    }

    #[test]
    fn shortest_path_case_to_cited_statute() {
        let conn_a = setup();
        let g = conn_a.lock();
        let p = shortest_path(
            &g,
            "case::2024노3424",
            "statute::근로기준법::109",
            3,
        )
        .unwrap()
        .unwrap();
        assert_eq!(p.first().unwrap(), "case::2024노3424");
        assert_eq!(p.last().unwrap(), "statute::근로기준법::109");
        assert!(p.len() <= 3);
    }

    #[test]
    fn shortest_path_same_node() {
        let conn_a = setup();
        let g = conn_a.lock();
        let p = shortest_path(&g, "statute::근로기준법::36", "statute::근로기준법::36", 3)
            .unwrap()
            .unwrap();
        assert_eq!(p, vec!["statute::근로기준법::36".to_string()]);
    }

    #[test]
    fn shortest_path_missing_node_returns_none() {
        let conn_a = setup();
        let g = conn_a.lock();
        let p =
            shortest_path(&g, "statute::근로기준법::36", "statute::없는법::1", 3).unwrap();
        assert!(p.is_none());
    }

    #[test]
    fn read_article_statute_returns_body_and_frontmatter() {
        let conn_a = setup();
        let g = conn_a.lock();
        let a = read_article(&g, "statute::근로기준법::43").unwrap().unwrap();
        assert_eq!(a.kind, NodeKind::Statute);
        assert!(a.content.contains("제43조"));
        assert_eq!(a.metadata.get("law_name").map(String::as_str), Some("근로기준법"));
        assert_eq!(a.metadata.get("article_key").map(String::as_str), Some("43"));
        assert!(a.sections.is_empty()); // statutes don't have ## sections
    }

    #[test]
    fn read_article_case_parses_sections() {
        let conn_a = setup();
        let g = conn_a.lock();
        let a = read_article(&g, "case::2024노3424").unwrap().unwrap();
        assert_eq!(a.kind, NodeKind::Case);
        assert!(a.sections.contains_key("판결요지"));
        assert!(a.sections.contains_key("참조조문"));
        assert_eq!(a.metadata.get("case_number").map(String::as_str), Some("2024노3424"));
    }

    #[test]
    fn read_article_missing_slug_returns_none() {
        let conn_a = setup();
        let g = conn_a.lock();
        assert!(read_article(&g, "statute::없는법::1").unwrap().is_none());
    }

    #[test]
    fn find_nodes_exact_slug() {
        let conn_a = setup();
        let g = conn_a.lock();
        let hits = find_nodes(&g, "statute::근로기준법::43", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].matched_via, "exact-slug");
    }

    #[test]
    fn find_nodes_exact_alias_human_form() {
        let conn_a = setup();
        let g = conn_a.lock();
        // Aliases generated by ingest include `근로기준법 제43조` (with space).
        let hits = find_nodes(&g, "근로기준법 제43조", 5).unwrap();
        assert!(
            hits.iter().any(|h| h.slug == "statute::근로기준법::43"),
            "got: {:?}",
            hits
        );
    }

    #[test]
    fn find_nodes_parses_natural_language_citation() {
        let conn_a = setup();
        let g = conn_a.lock();
        // Not an exact alias — but the citation regex should parse it.
        let hits = find_nodes(&g, "근로기준법 제36조 제1항", 5).unwrap();
        assert!(
            hits.iter().any(|h| h.slug == "statute::근로기준법::36"),
            "got: {:?}",
            hits
        );
    }

    #[test]
    fn find_nodes_parses_bare_case_number() {
        let conn_a = setup();
        let g = conn_a.lock();
        let hits = find_nodes(&g, "2024노3424", 5).unwrap();
        assert!(hits.iter().any(|h| h.slug == "case::2024노3424"));
    }

    #[test]
    fn find_nodes_empty_query_is_empty() {
        let conn_a = setup();
        let g = conn_a.lock();
        assert!(find_nodes(&g, "", 5).unwrap().is_empty());
        assert!(find_nodes(&g, "   ", 5).unwrap().is_empty());
    }

    /// Setup with multiple supplement nodes of the same law, so we can
    /// test version-picking logic.
    fn setup_with_multiple_supplements() -> Arc<Mutex<Connection>> {
        let c = Connection::open_in_memory().unwrap();
        init_schema(&c).unwrap();
        let conn = Arc::new(Mutex::new(c));
        let md = r#"# 근로기준법

```json
{
  "meta": {"lsNm": "근로기준법", "ancYd": "20251001", "efYd": "20251001"},
  "title": "근로기준법",
  "articles": [
    {"anchor": "J36:0", "number": "제36조(금품 청산)", "text": "제36조 14일 이내 지급."}
  ],
  "supplements": [
    {"title": "부칙  <법률 제10339호, 2010. 6. 4.>", "body": "이 법은 공포 후 6개월이 경과한 날부터 시행한다."},
    {"title": "부칙  <법률 제17326호, 2020. 5. 26.>", "body": "이 법은 공포한 날부터 시행한다."},
    {"title": "부칙  <법률 제20520호, 2024. 10. 22.>", "body": "제1조(시행일) 이 법은 2025년 10월 1일부터 시행한다."}
  ]
}
```
"#;
        let sdoc = extract_statute(md, "/x/20251001/근로기준법.md").unwrap();
        ingest_statute(&conn, &sdoc).unwrap();

        // Insert a fake case row with verdict_date = 2022-03-15.
        {
            let g = conn.lock();
            g.execute(
                "INSERT INTO vault_documents (uuid, title, content, source_type, source_device_id,
                                               checksum, doc_type, char_count, created_at, updated_at)
                 VALUES ('u1','case::2024노3424','body','local_file','local','c','case',5,1,1)",
                [],
            )
            .unwrap();
            let id: i64 = g
                .query_row(
                    "SELECT id FROM vault_documents WHERE title='case::2024노3424'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            g.execute(
                "INSERT INTO vault_frontmatter (doc_id, key, value) VALUES (?1, 'verdict_date', '20220315')",
                rusqlite::params![id],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn picker_plain_citation_uses_verdict_date_제1원칙() {
        let conn = setup_with_multiple_supplements();
        let g = conn.lock();
        let d = pick_applicable_version(&g, "case::2024노3424", "statute::근로기준법::36", None)
            .unwrap();
        assert_eq!(d.branch, "primary_verdict_date");
        assert_eq!(d.anchor_date.as_deref(), Some("20220315"));
        // Only the 2020-05-26 supplement has effective_date ≤ 2022-03-15.
        // (2010-12-04 = 공포 + 6개월 is earlier; 2020-05-26 is later.)
        assert_eq!(d.promulgation_no.as_deref(), Some("17326"));
        assert_eq!(d.effective_date.as_deref(), Some("20200526"));
    }

    #[test]
    fn picker_revision_cutoff_picks_version_before_cutoff_day() {
        let conn = setup_with_multiple_supplements();
        let g = conn.lock();
        // Cutoff 2020-05-26 means "pre-revision" → effective on or before 2020-05-25.
        let d = pick_applicable_version(
            &g,
            "case::2024노3424",
            "statute::근로기준법::36",
            Some("20200526"),
        )
        .unwrap();
        assert_eq!(d.branch, "revision_cutoff");
        assert_eq!(d.anchor_date.as_deref(), Some("20200525"));
        // Only 2010-12-04 qualifies.
        assert_eq!(d.promulgation_no.as_deref(), Some("10339"));
    }

    #[test]
    fn picker_fallback_latest_when_verdict_date_missing() {
        let conn = setup_with_multiple_supplements();
        let g = conn.lock();
        // Use a case without verdict_date.
        {
            let guard = &g;
            guard
                .execute(
                    "INSERT INTO vault_documents (uuid, title, content, source_type, source_device_id,
                                                   checksum, doc_type, char_count, created_at, updated_at)
                     VALUES ('u2','case::noverdict','body','local_file','local','c2','case',5,1,1)",
                    [],
                )
                .unwrap();
        }
        let d =
            pick_applicable_version(&g, "case::noverdict", "statute::근로기준법::36", None).unwrap();
        assert_eq!(d.branch, "fallback_latest");
        // Newest supplement by effective_date is 2025-10-01 (법률 제20520호).
        assert_eq!(d.promulgation_no.as_deref(), Some("20520"));
    }

    #[test]
    fn picker_returns_none_for_missing_statute() {
        let conn = setup_with_multiple_supplements();
        let g = conn.lock();
        let d = pick_applicable_version(&g, "case::2024노3424", "statute::없는법::1", None)
            .unwrap();
        assert_eq!(d.branch, "none");
        assert!(d.supplement_slug.is_none());
    }

    // ───── Cross-schema (domain.db ATTACH) tests — Step A4 ─────

    /// Build a Connection with both `main` (in-memory) and `domain`
    /// (file-backed via tempfile, ATTACHed) schemas + identical vault
    /// tables. Returns (conn, tempdir-keepalive).
    fn fresh_attached_conn() -> (Arc<Mutex<Connection>>, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let domain_path = tmp.path().join("domain.db");
        // Init schema on the domain file first so ATTACH sees a complete
        // table set.
        crate::vault::domain::ensure_schema(&domain_path).unwrap();

        let main = Connection::open_in_memory().unwrap();
        init_schema(&main).unwrap();
        crate::vault::domain::attach(&main, &domain_path).unwrap();
        assert!(crate::vault::domain::is_attached(&main).unwrap());
        (Arc::new(Mutex::new(main)), tmp)
    }

    #[test]
    fn cross_schema_get_node_finds_domain_only_slug() {
        let (conn, _tmp) = fresh_attached_conn();
        // Ingest the statute INTO domain only.
        let sdoc = crate::vault::legal::extract_statute(
            STATUTE_MD,
            "/x/20251001/근로기준법.md",
        )
        .unwrap();
        crate::vault::legal::ingest_statute_to(
            &conn,
            &sdoc,
            crate::vault::legal::IngestTarget::Domain,
        )
        .unwrap();

        let g = conn.lock();
        // Slug exists in domain, NOT in main — get_node must still find it.
        let main_count: i64 = g
            .query_row(
                "SELECT COUNT(*) FROM main.vault_documents WHERE title='statute::근로기준법::36'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(main_count, 0, "must not have written to main");
        let domain_count: i64 = g
            .query_row(
                "SELECT COUNT(*) FROM domain.vault_documents WHERE title='statute::근로기준법::36'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(domain_count, 1, "must have written to domain");

        let n = get_node(&g, "statute::근로기준법::36").unwrap().unwrap();
        assert_eq!(n.slug, "statute::근로기준법::36");
        assert_eq!(n.law_name.as_deref(), Some("근로기준법"));
    }

    #[test]
    fn cross_schema_neighbors_resolves_main_link_to_domain_node() {
        let (conn, _tmp) = fresh_attached_conn();
        // Ingest statute into DOMAIN.
        let sdoc = crate::vault::legal::extract_statute(
            STATUTE_MD,
            "/x/20251001/근로기준법.md",
        )
        .unwrap();
        crate::vault::legal::ingest_statute_to(
            &conn,
            &sdoc,
            crate::vault::legal::IngestTarget::Domain,
        )
        .unwrap();

        // Ingest case into MAIN — its `cites` edges target domain slugs.
        let cdoc = crate::vault::legal::extract_case(
            CASE_MD,
            "/x/20250530/2024노3424_400102_형사_606941.md",
        )
        .unwrap();
        crate::vault::legal::ingest_case_to(
            &conn,
            &cdoc,
            crate::vault::legal::IngestTarget::Main,
        )
        .unwrap();

        let g = conn.lock();
        // The case's edges have target_doc_id NULL (cross-schema → unresolved
        // by the writer) but target_raw set, so neighbors traversal resolves.
        let sg = neighbors(&g, "case::2024노3424", 1, &[]).unwrap();
        let slugs: HashSet<&str> = sg.nodes.iter().map(|n| n.slug.as_str()).collect();
        assert!(
            slugs.contains("statute::근로기준법::36"),
            "expected domain statute among neighbors of main case; got {slugs:?}"
        );
        assert!(
            slugs.contains("statute::근로기준법::109"),
            "expected domain statute 109 among neighbors of main case; got {slugs:?}"
        );
    }

    #[test]
    fn cross_schema_domain_priority_when_slug_collides() {
        let (conn, _tmp) = fresh_attached_conn();
        // Insert the SAME slug into main AND domain with different
        // content so we can tell them apart.
        {
            let g = conn.lock();
            // main copy.
            g.execute(
                "INSERT INTO main.vault_documents
                    (uuid, title, content, source_type, source_device_id, checksum,
                     doc_type, char_count, created_at, updated_at)
                 VALUES ('u-main','statute::근로기준법::36','MAIN BODY','local_file','local',
                         'cm','statute_article',9,1,1)",
                [],
            )
            .unwrap();
            // domain copy.
            g.execute(
                "INSERT INTO domain.vault_documents
                    (uuid, title, content, source_type, source_device_id, checksum,
                     doc_type, char_count, created_at, updated_at)
                 VALUES ('u-dom','statute::근로기준법::36','DOMAIN BODY','local_file','local',
                         'cd','statute_article',11,1,1)",
                [],
            )
            .unwrap();
        }
        let g = conn.lock();
        let a = read_article(&g, "statute::근로기준법::36").unwrap().unwrap();
        // Domain wins on slug conflict.
        assert!(
            a.content.contains("DOMAIN BODY"),
            "expected domain content, got: {}",
            a.content
        );
    }

    #[test]
    fn cross_schema_find_nodes_resolves_domain_alias() {
        let (conn, _tmp) = fresh_attached_conn();
        let sdoc = crate::vault::legal::extract_statute(
            STATUTE_MD,
            "/x/20251001/근로기준법.md",
        )
        .unwrap();
        crate::vault::legal::ingest_statute_to(
            &conn,
            &sdoc,
            crate::vault::legal::IngestTarget::Domain,
        )
        .unwrap();
        let g = conn.lock();
        // Alias `근로기준법 제36조` was inserted into domain only.
        let hits = find_nodes(&g, "근로기준법 제36조", 5).unwrap();
        assert!(
            hits.iter().any(|h| h.slug == "statute::근로기준법::36"),
            "got: {hits:?}"
        );
    }
}
