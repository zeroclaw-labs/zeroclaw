//! session_search tool — full-text search across past conversation transcripts.
//!
//! When the user asks about past conversations ("what did we discuss last week
//! about X?"), this tool queries chat_messages_fts to find matching sessions,
//! groups hits by session, and returns session metadata + top-matching snippets.

use super::traits::{Tool, ToolResult};
use crate::session_search::SessionSearchStore;
use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write;
use std::sync::Arc;

pub struct SessionSearchTool {
    store: Arc<SessionSearchStore>,
}

impl SessionSearchTool {
    pub fn new(store: Arc<SessionSearchStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for SessionSearchTool {
    fn name(&self) -> &str {
        "session_search"
    }

    fn description(&self) -> &str {
        "Full-text search across past conversation transcripts. \
         Use this when the user references prior conversations ('지난번에...', \
         'last week', 'what did we decide about X'). \
         Returns matching sessions with snippets and metadata."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search keywords or phrase. Omit to list recent sessions."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max sessions to return (default: 5)"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args.get("query").and_then(|v| v.as_str());
        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(5, |v| v as usize);

        // No query → list recent sessions (metadata only, no LLM cost)
        if query.is_none() || query.unwrap_or("").trim().is_empty() {
            let sessions = self.store.recent_sessions(limit)?;
            if sessions.is_empty() {
                return Ok(ToolResult {
                    success: true,
                    output: "No past sessions found.".into(),
                    error: None,
                });
            }
            let mut out = format!("Recent {} sessions:\n", sessions.len());
            for s in &sessions {
                let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(s.started_at, 0)
                    .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| s.started_at.to_string());
                let _ = writeln!(
                    out,
                    "- [{}] {} ({}, {})",
                    s.category.as_deref().unwrap_or("general"),
                    s.title.as_deref().unwrap_or("(untitled)"),
                    s.platform.as_deref().unwrap_or("app"),
                    ts
                );
            }
            return Ok(ToolResult {
                success: true,
                output: out,
                error: None,
            });
        }

        let q = query.unwrap();
        let hits = self.store.search_sessions(q, limit)?;
        if hits.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: format!("No sessions matched '{}'", q),
                error: None,
            });
        }

        let mut out = format!("Found {} matching sessions for '{}':\n\n", hits.len(), q);
        for h in &hits {
            let ts = chrono::DateTime::<chrono::Utc>::from_timestamp(h.session.started_at, 0)
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "## [{}] {} ({} matches)",
                ts,
                h.session.title.as_deref().unwrap_or("(untitled)"),
                h.match_count
            );
            for snippet in &h.snippets {
                let truncated = if snippet.chars().count() > 200 {
                    let end = snippet
                        .char_indices()
                        .nth(200)
                        .map(|(i, _)| i)
                        .unwrap_or(snippet.len());
                    format!("{}...", &snippet[..end])
                } else {
                    snippet.clone()
                };
                let _ = writeln!(out, "  - {}", truncated);
            }
            out.push('\n');
        }

        Ok(ToolResult {
            success: true,
            output: out,
            error: None,
        })
    }
}
