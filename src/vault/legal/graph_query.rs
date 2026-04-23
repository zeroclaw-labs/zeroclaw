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

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

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
            "statute_article" | "statute_supplement" => Some(Self::Statute),
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
/// its `doc_type` isn't in the legal set.
pub fn get_node(conn: &Connection, slug: &str) -> Result<Option<Node>> {
    let row = conn
        .query_row(
            "SELECT id, doc_type FROM vault_documents
              WHERE title = ?1 AND doc_type IN ('statute_article','statute_supplement','case')",
            params![slug],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .ok();
    let Some((id, doc_type)) = row else {
        return Ok(None);
    };
    let kind = NodeKind::from_doc_type(&doc_type).unwrap();
    Ok(Some(hydrate_node(conn, id, slug, kind)?))
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
    let row: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT id, doc_type, content FROM vault_documents
              WHERE title = ?1 AND doc_type IN ('statute_article','statute_supplement','case')",
            params![slug],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .ok();
    let Some((id, doc_type, content)) = row else {
        return Ok(None);
    };
    let kind = NodeKind::from_doc_type(&doc_type).unwrap();
    let metadata = load_frontmatter(conn, id)?;
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
    let alias_hit: Option<String> = conn
        .query_row(
            "SELECT d.title FROM vault_aliases va
               JOIN vault_documents d ON d.id = va.doc_id
              WHERE va.alias = ?1
                AND d.doc_type IN ('statute_article','statute_supplement','case')",
            params![q],
            |r| r.get::<_, String>(0),
        )
        .ok();
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
    if out.len() < limit && q.chars().count() >= 3 {
        let remaining = limit - out.len();
        let fts_query = fts_escape(q);
        if !fts_query.is_empty() {
            let mut stmt = conn.prepare(
                "SELECT d.title
                   FROM vault_docs_fts f JOIN vault_documents d ON d.id = f.rowid
                  WHERE vault_docs_fts MATCH ?1
                    AND d.doc_type IN ('statute_article','statute_supplement','case')
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
fn fetch_direct_neighbors(conn: &Connection, slug: &str) -> Result<Vec<String>> {
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM vault_documents WHERE title = ?1",
            params![slug],
            |r| r.get::<_, i64>(0),
        )
        .ok();
    let Some(id) = id else {
        return Ok(vec![]);
    };
    let mut out: Vec<String> = Vec::new();

    // Outbound: legal_node → whatever. Target slug is the target_raw
    // (always set, even for unresolved links).
    {
        let mut stmt = conn.prepare(
            "SELECT target_raw FROM vault_links WHERE source_doc_id = ?1",
        )?;
        let rows = stmt.query_map(params![id], |r| r.get::<_, String>(0))?;
        for r in rows {
            out.push(r?);
        }
    }

    // Inbound: whatever → legal_node. Prefer target_doc_id match; for
    // unresolved links where target_raw == slug, those are caught too.
    {
        let mut stmt = conn.prepare(
            "SELECT d.title
               FROM vault_links vl
               JOIN vault_documents d ON d.id = vl.source_doc_id
              WHERE (vl.target_doc_id = ?1 AND vl.is_resolved = 1)
                 OR (vl.target_raw = ?2 AND vl.is_resolved = 0)",
        )?;
        let rows = stmt.query_map(params![id, slug], |r| r.get::<_, String>(0))?;
        for r in rows {
            out.push(r?);
        }
    }

    // Restrict to legal nodes only (other vault docs may share brain.db).
    if out.is_empty() {
        return Ok(out);
    }
    let placeholders = std::iter::repeat("?")
        .take(out.len())
        .collect::<Vec<_>>()
        .join(",");
    let q = format!(
        "SELECT title FROM vault_documents
           WHERE title IN ({placeholders})
             AND doc_type IN ('statute_article','statute_supplement','case')"
    );
    let mut stmt = conn.prepare(&q)?;
    let params_dyn: Vec<&dyn rusqlite::ToSql> =
        out.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let rows = stmt.query_map(params_dyn.as_slice(), |r| r.get::<_, String>(0))?;
    let keep: HashSet<String> = rows.filter_map(Result::ok).collect();
    out.retain(|s| keep.contains(s));
    out.sort();
    out.dedup();
    Ok(out)
}

/// All edges whose both endpoints are in `slugs`.
fn fetch_edges_within(conn: &Connection, slugs: &[String]) -> Result<Vec<Edge>> {
    if slugs.is_empty() {
        return Ok(vec![]);
    }
    let placeholders = std::iter::repeat("?")
        .take(slugs.len())
        .collect::<Vec<_>>()
        .join(",");
    let q = format!(
        "SELECT src.title, vl.target_raw, vl.display_text, vl.context, vl.is_resolved
           FROM vault_links vl
           JOIN vault_documents src ON src.id = vl.source_doc_id
          WHERE src.title IN ({ph}) AND vl.target_raw IN ({ph})
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
        Ok(Edge {
            source_slug: r.get(0)?,
            target_slug: r.get(1)?,
            relation: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            evidence: r.get::<_, Option<String>>(3)?,
            resolved: r.get::<_, i64>(4)? != 0,
        })
    })?;
    let mut edges: Vec<Edge> = rows.filter_map(Result::ok).collect();
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

fn hydrate_node(conn: &Connection, id: i64, slug: &str, kind: NodeKind) -> Result<Node> {
    let fm = load_frontmatter(conn, id)?;
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
    })
}

fn load_frontmatter(conn: &Connection, doc_id: i64) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare(
        "SELECT key, value FROM vault_frontmatter WHERE doc_id = ?1",
    )?;
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
}
