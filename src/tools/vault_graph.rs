//! Agent tools exposing the legal second-brain graph.
//!
//! Three tools, all read-only, backed by `src/vault/legal/graph_query.rs`:
//!   - [`LegalGraphNeighborsTool`] — n-hop neighbors of a node, human-summarised.
//!   - [`LegalGraphShortestPathTool`] — BFS shortest path between two nodes.
//!   - [`LegalGraphSubgraphTool`] — graphify-compatible `{nodes, edges}` JSON.
//!
//! Input/output contract is schema-documented so the cloud LLM (and the
//! on-device SLM via `safe_for_slm=true`) can call these reliably.
//!
//! All tools open a fresh read-only connection per call against
//! `<workspace_dir>/memory/brain.db`; SQLite WAL mode handles concurrent
//! readers so this is cheaper than plumbing through `VaultStore`.

use super::traits::{Tool, ToolResult};
use crate::vault::legal::graph_query::{
    self, ApplicableVersion, ArticleContent, Edge, FindHit, Node, NodeKind, Subgraph, MAX_NODES,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;

// ───────── Neighbors ─────────

pub struct LegalGraphNeighborsTool {
    workspace_dir: Arc<PathBuf>,
}

impl LegalGraphNeighborsTool {
    pub fn new(workspace_dir: Arc<PathBuf>) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for LegalGraphNeighborsTool {
    fn name(&self) -> &str {
        "legal_graph_neighbors"
    }

    fn description(&self) -> &str {
        "Explore the legal second-brain graph. Given a node slug like `statute::근로기준법::36` or `case::2024노3424`, returns all nodes reachable within `depth` hops (both cited-by and citing direction). Use to answer questions like \"which precedents rely on this statute article\" or \"what statutes does this case cite\"."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "node": {
                    "type": "string",
                    "description": "Canonical node slug. Statute: `statute::{법령명}::{조번호}` (e.g. `statute::근로기준법::43-2`). Case: `case::{사건번호}` (e.g. `case::2024노3424`)."
                },
                "depth": {
                    "type": "integer",
                    "description": "Hop limit (default 1, max 3). Keep small — depth 2+ on a well-connected node can reach hundreds of articles.",
                    "minimum": 1,
                    "maximum": 3,
                    "default": 1
                },
                "kinds": {
                    "type": "array",
                    "description": "Optional filter: only include nodes of these kinds. Omit for both. Values: `statute` / `case`.",
                    "items": { "type": "string", "enum": ["statute","case"] }
                }
            },
            "required": ["node"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let node = arg_str(&args, "node")?;
        let depth = arg_int(&args, "depth", 1, 1, 3);
        let kinds = arg_kinds(&args);

        let workspace = self.workspace_dir.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<Subgraph> {
            let conn = open_brain_db(&workspace)?;
            graph_query::neighbors(&conn, &node, depth, &kinds)
        })
        .await
        .context("neighbors task join")??;

        Ok(format_neighbors(&args, &result))
    }
}

fn format_neighbors(args: &Value, sg: &Subgraph) -> ToolResult {
    let root = args.get("node").and_then(Value::as_str).unwrap_or("?");
    let mut out = String::new();
    let _ = writeln!(
        out,
        "root: {root}  |  nodes: {}  |  edges: {}{}",
        sg.nodes.len(),
        sg.edges.len(),
        if sg.truncated {
            format!("  (truncated at {MAX_NODES})")
        } else {
            String::new()
        }
    );
    if sg.nodes.is_empty() {
        let _ = writeln!(out, "(no nodes — check slug or depth)");
    } else {
        for n in &sg.nodes {
            let _ = writeln!(out, "  [{}] {} — {}", short_kind(n.kind), n.slug, n.label);
        }
    }
    if !sg.edges.is_empty() {
        out.push_str("edges:\n");
        for e in sg.edges.iter().take(50) {
            let _ = writeln!(
                out,
                "  {} --{}--> {}{}",
                e.source_slug,
                e.relation,
                e.target_slug,
                if e.resolved { "" } else { "  (unresolved)" },
            );
        }
        if sg.edges.len() > 50 {
            let _ = writeln!(out, "  … +{} more edges", sg.edges.len() - 50);
        }
    }
    ToolResult {
        success: true,
        output: out,
        error: None,
    }
}

// ───────── Shortest path ─────────

pub struct LegalGraphShortestPathTool {
    workspace_dir: Arc<PathBuf>,
}

impl LegalGraphShortestPathTool {
    pub fn new(workspace_dir: Arc<PathBuf>) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for LegalGraphShortestPathTool {
    fn name(&self) -> &str {
        "legal_graph_shortest_path"
    }

    fn description(&self) -> &str {
        "Find the shortest chain of legal references between two nodes (statute articles or cases) in the second-brain graph. Useful for \"how is case X connected to statute Y\". Returns `null` if no path within `max_depth` hops."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "description": "Source slug (e.g. `case::2024노3424`)." },
                "to":   { "type": "string", "description": "Target slug (e.g. `statute::근로기준법::109`)." },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum hops to search (default 4, max 6).",
                    "minimum": 1,
                    "maximum": 6,
                    "default": 4
                }
            },
            "required": ["from", "to"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let from = arg_str(&args, "from")?;
        let to = arg_str(&args, "to")?;
        let max_depth = arg_int(&args, "max_depth", 4, 1, 6);
        let workspace = self.workspace_dir.clone();

        let path = tokio::task::spawn_blocking(move || -> Result<Option<Vec<String>>> {
            let conn = open_brain_db(&workspace)?;
            graph_query::shortest_path(&conn, &from, &to, max_depth)
        })
        .await
        .context("shortest_path task join")??;

        let output = match path {
            None => "(no path within max_depth)".to_string(),
            Some(p) => {
                let mut s = format!("hops: {}\n", p.len().saturating_sub(1));
                s.push_str(&p.join("\n  -> "));
                s
            }
        };

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

// ───────── Subgraph (JSON, graphify-compatible) ─────────

pub struct LegalGraphSubgraphTool {
    workspace_dir: Arc<PathBuf>,
}

impl LegalGraphSubgraphTool {
    pub fn new(workspace_dir: Arc<PathBuf>) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for LegalGraphSubgraphTool {
    fn name(&self) -> &str {
        "legal_graph_subgraph"
    }

    fn description(&self) -> &str {
        "Dump a graphify-compatible JSON subgraph (`{nodes, edges}`) rooted at a node. For programmatic handoff to the Cytoscape viewer or snapshot exporter. Structurally identical to `legal_graph_neighbors` but returns the full structured payload instead of a human summary."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "node":  { "type": "string", "description": "Root slug." },
                "depth": { "type": "integer", "minimum": 1, "maximum": 3, "default": 1 },
                "kinds": {
                    "type": "array",
                    "items": { "type": "string", "enum": ["statute","case"] }
                }
            },
            "required": ["node"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let node = arg_str(&args, "node")?;
        let depth = arg_int(&args, "depth", 1, 1, 3);
        let kinds = arg_kinds(&args);
        let workspace = self.workspace_dir.clone();

        let sg = tokio::task::spawn_blocking(move || -> Result<Subgraph> {
            let conn = open_brain_db(&workspace)?;
            graph_query::neighbors(&conn, &node, depth, &kinds)
        })
        .await
        .context("subgraph task join")??;

        let payload = serde_json::to_string(&sg).context("serialising subgraph")?;
        Ok(ToolResult {
            success: true,
            output: payload,
            error: None,
        })
    }
}

// ───────── Find (slug lookup from human-readable query) ─────────

pub struct LegalGraphFindTool {
    workspace_dir: Arc<PathBuf>,
}

impl LegalGraphFindTool {
    pub fn new(workspace_dir: Arc<PathBuf>) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for LegalGraphFindTool {
    fn name(&self) -> &str {
        "legal_graph_find"
    }

    fn description(&self) -> &str {
        "Resolve a human-readable legal reference to its canonical slug. Accepts natural forms like `민법 제839조의2`, `근로기준법 제43조의2(체불사업주 명단 공개)`, bare case numbers like `2024노3424`, or a canonical slug. Returns the best matches with a `matched_via` tag (`exact-slug` / `exact-alias` / `parsed-citation` / `parsed-case-number` / `fts-fallback`) so the agent can judge confidence before calling neighbors/read."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Citation text or case number as a lawyer would write it."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max suggestions (default 5, max 20).",
                    "minimum": 1, "maximum": 20, "default": 5
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let query = arg_str(&args, "query")?;
        let limit = arg_int(&args, "limit", 5, 1, 20);
        let workspace = self.workspace_dir.clone();

        let hits = tokio::task::spawn_blocking(move || -> Result<Vec<FindHit>> {
            let conn = open_brain_db(&workspace)?;
            graph_query::find_nodes(&conn, &query, limit)
        })
        .await
        .context("find_nodes task join")??;

        let mut out = String::new();
        if hits.is_empty() {
            out.push_str("(no matches — try the canonical slug or a less specific form)");
        } else {
            let _ = writeln!(out, "{} hit(s):", hits.len());
            for h in &hits {
                let _ = writeln!(
                    out,
                    "  [{}] {} ({}) — {}",
                    short_kind(h.kind),
                    h.slug,
                    h.matched_via,
                    h.label
                );
            }
        }
        Ok(ToolResult {
            success: true,
            output: out,
            error: None,
        })
    }
}

// ───────── Read article (full body + metadata + case sections) ─────────

pub struct LegalReadArticleTool {
    workspace_dir: Arc<PathBuf>,
}

impl LegalReadArticleTool {
    pub fn new(workspace_dir: Arc<PathBuf>) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for LegalReadArticleTool {
    fn name(&self) -> &str {
        "legal_read_article"
    }

    fn description(&self) -> &str {
        "Read the full body of a legal node by slug. For statutes returns the article header + body text + frontmatter (법령명, 조번호, 공포일/시행일). For cases returns the full markdown plus a parsed section map (판시사항, 판결요지, 참조조문, 참조판례, 판례내용). Use this BEFORE quoting legal language so the agent can cite verbatim."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "slug": {
                    "type": "string",
                    "description": "Canonical slug. Get one from `legal_graph_find` or `legal_graph_neighbors` if you don't already have it."
                },
                "sections": {
                    "type": "array",
                    "description": "For cases only: return just these ## sections (e.g. `[\"판결요지\",\"참조조문\"]`). Omit to get the full markdown.",
                    "items": { "type": "string" }
                }
            },
            "required": ["slug"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let slug = arg_str(&args, "slug")?;
        let filter: Vec<String> = args
            .get("sections")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let workspace = self.workspace_dir.clone();
        let slug_for_err = slug.clone();

        let article = tokio::task::spawn_blocking(move || -> Result<Option<ArticleContent>> {
            let conn = open_brain_db(&workspace)?;
            graph_query::read_article(&conn, &slug)
        })
        .await
        .context("read_article task join")??;

        let Some(a) = article else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("slug not found: {slug_for_err}")),
            });
        };

        let mut out = String::new();
        let _ = writeln!(out, "slug: {}", a.slug);
        let _ = writeln!(out, "kind: {}", a.kind.as_doc_type());
        let _ = writeln!(out, "label: {}", a.label);
        if !a.metadata.is_empty() {
            out.push_str("metadata:\n");
            let mut keys: Vec<&String> = a.metadata.keys().collect();
            keys.sort();
            for k in keys {
                if let Some(v) = a.metadata.get(k) {
                    if !v.is_empty() {
                        let _ = writeln!(out, "  {k}: {v}");
                    }
                }
            }
        }
        if matches!(a.kind, NodeKind::Case) && !a.sections.is_empty() {
            out.push_str("sections:\n");
            let mut keys: Vec<&String> = a.sections.keys().collect();
            keys.sort();
            for k in keys {
                if !filter.is_empty() && !filter.iter().any(|f| f == k) {
                    continue;
                }
                if let Some(v) = a.sections.get(k) {
                    let _ = writeln!(out, "\n## {k}\n{v}");
                }
            }
        } else {
            out.push_str("\n--- content ---\n");
            out.push_str(&a.content);
        }

        Ok(ToolResult {
            success: true,
            output: out,
            error: None,
        })
    }
}

// ───────── Applicable-version picker (2-tier 제1원칙) ─────────

pub struct LegalApplicableVersionTool {
    workspace_dir: Arc<PathBuf>,
}

impl LegalApplicableVersionTool {
    pub fn new(workspace_dir: Arc<PathBuf>) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for LegalApplicableVersionTool {
    fn name(&self) -> &str {
        "legal_applicable_version"
    }

    fn description(&self) -> &str {
        "Pick the applicable version of a Korean statute article for a given case, using the 2-tier 제1원칙: \
(1) If the case's citation to this article had `구 {법}(... 개정되기 전의 것)` → apply the version effective immediately BEFORE the cutoff. \
(2) Otherwise → apply the version in force at the case's verdict_date (판결 선고 시점). \
Returns the chosen supplement slug, its effective_date / promulgation info, the anchor date used, and a short human explanation. `branch` = `primary_verdict_date` | `revision_cutoff` | `fallback_latest` | `none` tells you which rule fired."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "case_slug":    { "type": "string", "description": "e.g. `case::2024노3424`" },
                "statute_slug": { "type": "string", "description": "e.g. `statute::근로기준법::36`" },
                "edge_cutoff":  {
                    "type": "string",
                    "description": "Optional explicit revision cutoff (YYYYMMDD). If omitted, the tool looks up the citation edge in brain.db and uses its stored `revision_cutoff`. Pass explicitly when overriding or when the edge wasn't ingested."
                }
            },
            "required": ["case_slug", "statute_slug"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let case_slug = arg_str(&args, "case_slug")?;
        let statute_slug = arg_str(&args, "statute_slug")?;
        let explicit_cutoff: Option<String> = args
            .get("edge_cutoff")
            .and_then(Value::as_str)
            .filter(|s| s.len() == 8)
            .map(str::to_string);
        let workspace = self.workspace_dir.clone();

        let decision = tokio::task::spawn_blocking(move || -> Result<ApplicableVersion> {
            let conn = open_brain_db(&workspace)?;
            // If the caller didn't supply an explicit cutoff, look the
            // edge up in-database.
            let cutoff: Option<String> = explicit_cutoff.or_else(|| {
                conn.query_row(
                    "SELECT vl.context FROM vault_links vl
                       JOIN vault_documents src ON src.id = vl.source_doc_id
                      WHERE src.title = ?1 AND vl.target_raw = ?2
                      LIMIT 1",
                    rusqlite::params![case_slug, statute_slug],
                    |r| r.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
                .and_then(|ctx| {
                    ctx.strip_prefix("[pre:").and_then(|rest| {
                        rest.find("] ")
                            .map(|end| rest[..end].to_string())
                            .filter(|d| d.len() == 8 && d.chars().all(|c| c.is_ascii_digit()))
                    })
                })
            });
            graph_query::pick_applicable_version(&conn, &case_slug, &statute_slug, cutoff.as_deref())
        })
        .await
        .context("applicable_version task join")??;

        let payload = serde_json::to_string(&decision).context("serialising decision")?;
        Ok(ToolResult {
            success: true,
            output: payload,
            error: None,
        })
    }
}

// ───────── Helpers ─────────

fn open_brain_db(workspace_dir: &PathBuf) -> Result<Connection> {
    let db_path = workspace_dir.join("memory").join("brain.db");
    if !db_path.exists() {
        anyhow::bail!(
            "brain.db not found at {} — run `zeroclaw vault legal ingest <dir>` first",
            db_path.display()
        );
    }
    let conn = Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening {} read-only", db_path.display()))?;
    // Ensure schema tables exist; if not, fail fast.
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='vault_documents'",
        [],
        |_| Ok(()),
    )
    .context("vault_documents table missing — has the vault been initialised?")?;
    Ok(conn)
}

fn arg_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("missing string arg `{key}`"))
}

fn arg_int(args: &Value, key: &str, default: i64, min: i64, max: i64) -> usize {
    let raw = args.get(key).and_then(Value::as_i64).unwrap_or(default);
    raw.clamp(min, max) as usize
}

fn arg_kinds(args: &Value) -> Vec<NodeKind> {
    args.get("kinds")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .filter_map(|s| match s {
                    "statute" => Some(NodeKind::Statute),
                    "case" => Some(NodeKind::Case),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn short_kind(k: NodeKind) -> &'static str {
    match k {
        NodeKind::Statute => "S",
        NodeKind::Case => "C",
    }
}

#[cfg(not(any(unix, windows)))]
compile_error!("vault_graph tools require std::path support");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::legal::{extract_case, extract_statute, ingest_case, ingest_statute};
    use crate::vault::schema::init_schema;
    use parking_lot::Mutex;
    use rusqlite::Connection;
    use std::sync::Arc;
    use tempfile::TempDir;

    const STATUTE_MD: &str = r#"# 근로기준법

```json
{
  "meta": {"lsNm": "근로기준법", "ancYd": "20251001", "efYd": "20251001"},
  "title": "근로기준법",
  "articles": [
    {"anchor": "J36:0", "number": "제36조(금품 청산)", "text": "제36조(금품 청산) 14일 이내 지급."},
    {"anchor": "J109:0", "number": "제109조(벌칙)", "text": "제109조 제36조를 위반한 자는 처벌한다."}
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
## 판례내용
body
## 판결요지
summary
## 판시사항
holding
"#;

    fn seeded_workspace() -> (TempDir, Arc<PathBuf>) {
        let tmp = TempDir::new().unwrap();
        let mem_dir = tmp.path().join("memory");
        std::fs::create_dir_all(&mem_dir).unwrap();
        let db_path = mem_dir.join("brain.db");
        let conn = Connection::open(&db_path).unwrap();
        init_schema(&conn).unwrap();
        let shared: Arc<Mutex<Connection>> = Arc::new(Mutex::new(conn));

        let sdoc =
            extract_statute(STATUTE_MD, "/x/20251001/근로기준법.md").unwrap();
        ingest_statute(&shared, &sdoc).unwrap();
        let cdoc =
            extract_case(CASE_MD, "/x/20250530/2024노3424_400102_형사_606941.md").unwrap();
        ingest_case(&shared, &cdoc).unwrap();
        drop(shared); // release lock before tool opens read-only.

        let ws = Arc::new(tmp.path().to_path_buf());
        (tmp, ws)
    }

    #[tokio::test]
    async fn neighbors_tool_summarises_reachable_nodes() {
        let (_tmp, ws) = seeded_workspace();
        let tool = LegalGraphNeighborsTool::new(ws);
        let r = tool
            .execute(json!({ "node": "statute::근로기준법::36", "depth": 1 }))
            .await
            .unwrap();
        assert!(r.success);
        assert!(r.output.contains("case::2024노3424"));
        assert!(r.output.contains("statute::근로기준법::109"));
    }

    #[tokio::test]
    async fn neighbors_tool_kinds_filter_excludes_cases() {
        let (_tmp, ws) = seeded_workspace();
        let tool = LegalGraphNeighborsTool::new(ws);
        let r = tool
            .execute(json!({
                "node": "statute::근로기준법::36",
                "depth": 1,
                "kinds": ["statute"]
            }))
            .await
            .unwrap();
        assert!(r.success);
        assert!(!r.output.contains("case::2024노3424"));
    }

    #[tokio::test]
    async fn shortest_path_tool_returns_chain_or_null() {
        let (_tmp, ws) = seeded_workspace();
        let tool = LegalGraphShortestPathTool::new(ws);
        let r = tool
            .execute(json!({
                "from": "case::2024노3424",
                "to":   "statute::근로기준법::109",
                "max_depth": 3
            }))
            .await
            .unwrap();
        assert!(r.success);
        assert!(r.output.contains("case::2024노3424"));
        assert!(r.output.contains("statute::근로기준법::109"));
        assert!(r.output.contains("->"));
    }

    #[tokio::test]
    async fn subgraph_tool_emits_valid_graphify_json() {
        let (_tmp, ws) = seeded_workspace();
        let tool = LegalGraphSubgraphTool::new(ws);
        let r = tool
            .execute(json!({ "node": "statute::근로기준법::36", "depth": 1 }))
            .await
            .unwrap();
        assert!(r.success);
        let v: serde_json::Value = serde_json::from_str(&r.output).unwrap();
        assert!(v.get("nodes").is_some());
        assert!(v.get("edges").is_some());
        assert!(v["nodes"].is_array());
        assert!(v["edges"].is_array());
    }

    #[tokio::test]
    async fn neighbors_tool_rejects_missing_node_arg() {
        let (_tmp, ws) = seeded_workspace();
        let tool = LegalGraphNeighborsTool::new(ws);
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("node"));
    }

    #[tokio::test]
    async fn find_tool_resolves_natural_language_citation() {
        let (_tmp, ws) = seeded_workspace();
        let tool = LegalGraphFindTool::new(ws);
        let r = tool
            .execute(json!({ "query": "근로기준법 제36조" }))
            .await
            .unwrap();
        assert!(r.success);
        assert!(r.output.contains("statute::근로기준법::36"));
    }

    #[tokio::test]
    async fn find_tool_resolves_bare_case_number() {
        let (_tmp, ws) = seeded_workspace();
        let tool = LegalGraphFindTool::new(ws);
        let r = tool
            .execute(json!({ "query": "2024노3424" }))
            .await
            .unwrap();
        assert!(r.success);
        assert!(r.output.contains("case::2024노3424"));
    }

    #[tokio::test]
    async fn read_article_tool_returns_statute_body() {
        let (_tmp, ws) = seeded_workspace();
        let tool = LegalReadArticleTool::new(ws);
        let r = tool
            .execute(json!({ "slug": "statute::근로기준법::109" }))
            .await
            .unwrap();
        assert!(r.success);
        assert!(r.output.contains("제109조"));
        assert!(r.output.contains("law_name: 근로기준법"));
    }

    #[tokio::test]
    async fn read_article_tool_filters_case_sections() {
        let (_tmp, ws) = seeded_workspace();
        let tool = LegalReadArticleTool::new(ws);
        let r = tool
            .execute(json!({
                "slug": "case::2024노3424",
                "sections": ["참조조문"]
            }))
            .await
            .unwrap();
        assert!(r.success);
        // Filtered to just 참조조문 — 판결요지 and 판례내용 must be absent.
        assert!(r.output.contains("## 참조조문"));
        assert!(!r.output.contains("## 판결요지"));
        assert!(!r.output.contains("## 판례내용"));
    }

    #[tokio::test]
    async fn read_article_tool_errors_on_missing_slug() {
        let (_tmp, ws) = seeded_workspace();
        let tool = LegalReadArticleTool::new(ws);
        let r = tool
            .execute(json!({ "slug": "statute::없는법::1" }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("없는법"));
    }
}
