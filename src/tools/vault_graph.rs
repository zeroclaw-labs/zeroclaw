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
    self, Edge, Node, NodeKind, Subgraph, MAX_NODES,
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
}
