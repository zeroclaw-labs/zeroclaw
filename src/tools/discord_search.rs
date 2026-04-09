use super::traits::{Tool, ToolResult};
use crate::memory::Memory;
use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write;
use std::sync::Arc;

/// Search Discord message history stored in discord.db.
pub struct DiscordSearchTool {
    discord_memory: Arc<dyn Memory>,
}

impl DiscordSearchTool {
    pub fn new(discord_memory: Arc<dyn Memory>) -> Self {
        Self { discord_memory }
    }
}

#[async_trait]
impl Tool for DiscordSearchTool {
    fn name(&self) -> &str {
        "discord_search"
    }

    fn description(&self) -> &str {
        "Search Discord message history. Returns messages matching a keyword query, optionally filtered by channel_id, author_id, or time range."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords or phrase to search for in Discord messages (optional if since/until provided)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results to return (default: 10)"
                },
                "channel_id": {
                    "type": "string",
                    "description": "Filter results to a specific Discord channel ID"
                },
                "since": {
                    "type": "string",
                    "description": "Filter messages at or after this time (RFC 3339, e.g. 2025-03-01T00:00:00Z)"
                },
                "until": {
                    "type": "string",
                    "description": "Filter messages at or before this time (RFC 3339)"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let channel_id = args.get("channel_id").and_then(|v| v.as_str());
        let since = args.get("since").and_then(|v| v.as_str());
        let until = args.get("until").and_then(|v| v.as_str());

        if query.trim().is_empty() && since.is_none() && until.is_none() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Provide at least 'query' (keywords) or time range ('since'/'until')".into(),
                ),
            });
        }

        if let Some(s) = since {
            if chrono::DateTime::parse_from_rfc3339(s).is_err() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Invalid 'since' date: {s}. Expected RFC 3339, e.g. 2025-03-01T00:00:00Z"
                    )),
                });
            }
        }
        if let Some(u) = until {
            if chrono::DateTime::parse_from_rfc3339(u).is_err() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Invalid 'until' date: {u}. Expected RFC 3339, e.g. 2025-03-01T00:00:00Z"
                    )),
                });
            }
        }
        if let (Some(s), Some(u)) = (since, until) {
            if let (Ok(s_dt), Ok(u_dt)) = (
                chrono::DateTime::parse_from_rfc3339(s),
                chrono::DateTime::parse_from_rfc3339(u),
            ) {
                if s_dt >= u_dt {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("'since' must be before 'until'".into()),
                    });
                }
            }
        }

        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(10, |v| v as usize);

        match self
            .discord_memory
            .recall(query, limit, channel_id, since, until)
            .await
        {
            Ok(entries) if entries.is_empty() => Ok(ToolResult {
                success: true,
                output: "No Discord messages found.".into(),
                error: None,
            }),
            Ok(entries) => {
                let mut output = format!("Found {} Discord messages:\n", entries.len());
                for entry in &entries {
                    let score = entry
                        .score
                        .map_or_else(String::new, |s| format!(" [{:.0}%]", s * 100.0));
                    let _ = writeln!(output, "- {}{score}", entry.content);
                }
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Discord search failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryCategory, SqliteMemory};
    use tempfile::TempDir;

    fn seeded_discord_mem() -> (TempDir, Arc<dyn Memory>) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new_named(tmp.path(), "discord").unwrap();
        (tmp, Arc::new(mem))
    }

    #[tokio::test]
    async fn search_empty() {
        let (_tmp, mem) = seeded_discord_mem();
        let tool = DiscordSearchTool::new(mem);
        let result = tool.execute(json!({"query": "hello"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No Discord messages found"));
    }

    #[tokio::test]
    async fn search_finds_match() {
        let (_tmp, mem) = seeded_discord_mem();
        mem.store(
            "discord_001",
            "@user1 in #general at 2025-01-01T00:00:00Z: hello world",
            MemoryCategory::Custom("discord".to_string()),
            Some("general"),
        )
        .await
        .unwrap();

        let tool = DiscordSearchTool::new(mem);
        let result = tool.execute(json!({"query": "hello"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("hello"));
    }

    #[tokio::test]
    async fn search_requires_query_or_time() {
        let (_tmp, mem) = seeded_discord_mem();
        let tool = DiscordSearchTool::new(mem);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("at least"));
    }

    #[test]
    fn name_and_schema() {
        let (_tmp, mem) = seeded_discord_mem();
        let tool = DiscordSearchTool::new(mem);
        assert_eq!(tool.name(), "discord_search");
        assert!(tool.parameters_schema()["properties"]["query"].is_object());
    }
}
