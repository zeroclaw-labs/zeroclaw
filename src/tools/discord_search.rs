use super::traits::{Tool, ToolResult};
use crate::memory::{Memory, MemoryCategory};
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
                "channel_name": {
                    "type": "string",
                    "description": "Filter by channel name (e.g. 'general', 'development')"
                },
                "username": {
                    "type": "string",
                    "description": "Filter by sender username"
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

        let channel_name = args.get("channel_name").and_then(|v| v.as_str());
        let username = args.get("username").and_then(|v| v.as_str());

        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(10, |v| v as usize);

        // When channel_name is given without an explicit channel_id, resolve matching
        // channel IDs from the channel_cache (content = channel name, session_id = channel_id).
        // This handles emoji suffixes and other name variations without relying on
        // post-fetch string matching against message content.
        let resolved_channel_ids: Vec<String> = if channel_name.is_some() && channel_id.is_none() {
            let c_name_lower = channel_name
                .unwrap_or("")
                .trim_start_matches('#')
                .to_lowercase();
            let cache_cat = MemoryCategory::Custom("channel_cache".to_string());
            self.discord_memory
                .list(Some(&cache_cat), None)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.content.to_lowercase().contains(&c_name_lower))
                .filter_map(|e| e.session_id)
                .collect()
        } else {
            vec![]
        };

        let entries = if resolved_channel_ids.is_empty() {
            // Fallback: text-based recall. If channel_name or username filters are
            // active, fetch extra entries to compensate for post-fetch filtering.
            let fetch_limit = if channel_name.is_some() || username.is_some() {
                limit * 5
            } else {
                limit
            };
            match self
                .discord_memory
                .recall(query, fetch_limit, channel_id, since, until)
                .await
            {
                Ok(e) => e,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Discord search failed: {e}")),
                    })
                }
            }
        } else {
            // Recall messages for each matching channel directly by session_id.
            // Fetch up to limit*2 per channel so truncation doesn't hide results.
            let per_channel = (limit * 2).max(10);
            let mut all = Vec::new();
            for cid in &resolved_channel_ids {
                if let Ok(ch_entries) = self
                    .discord_memory
                    .recall(query, per_channel, Some(cid.as_str()), since, until)
                    .await
                {
                    all.extend(ch_entries);
                }
            }
            // If query-filtered search returned nothing, fall back to all messages
            // from those channels. This handles cases where the user asks a meta-
            // question like "what was discussed" rather than searching specific content.
            if all.is_empty() && !query.trim().is_empty() {
                for cid in &resolved_channel_ids {
                    if let Ok(ch_entries) = self
                        .discord_memory
                        .recall("", per_channel, Some(cid.as_str()), since, until)
                        .await
                    {
                        all.extend(ch_entries);
                    }
                }
            }
            // Sort newest-first for consistent output across multiple channels.
            all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
            all
        };

        let mut filtered_entries = entries;

        // Apply channel_name text filter only on the fallback path (no cache-resolved IDs).
        if resolved_channel_ids.is_empty() {
            if let Some(c_name) = channel_name {
                let c_name_lower = c_name.trim_start_matches('#').to_lowercase();
                filtered_entries.retain(|e| {
                    let key_lower = e.key.to_lowercase();
                    let content_lower = e.content.to_lowercase();
                    key_lower.contains(&format!("#{}", c_name_lower))
                        || key_lower.contains(&c_name_lower)
                        || content_lower.contains(&format!("#{}", c_name_lower))
                        || content_lower.contains(&c_name_lower)
                });
            }
        }

        if let Some(u_name) = username {
            let u_name_lower = u_name.to_lowercase();
            filtered_entries.retain(|e| {
                e.key.to_lowercase().contains(&u_name_lower)
                    || e.content.to_lowercase().contains(&u_name_lower)
            });
        }

        filtered_entries.truncate(limit);

        if filtered_entries.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No Discord messages found matching those filters.".into(),
                error: None,
            });
        }

        let mut output = format!("Found {} Discord messages:\n", filtered_entries.len());
        for entry in &filtered_entries {
            let score = entry
                .score
                .map_or_else(String::new, |s| format!(" [{:.0}%]", s * 100.0));
            let _ = writeln!(
                output,
                "- {} (from {}): {}{score}",
                entry.timestamp, entry.key, entry.content
            );
        }
        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
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
    async fn search_empty_query_allowed() {
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
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("hello world"));
    }

    #[test]
    fn name_and_schema() {
        let (_tmp, mem) = seeded_discord_mem();
        let tool = DiscordSearchTool::new(mem);
        assert_eq!(tool.name(), "discord_search");
        assert!(tool.parameters_schema()["properties"]["query"].is_object());
    }

    #[tokio::test]
    async fn search_channel_name_resolves_via_cache() {
        let (_tmp, mem) = seeded_discord_mem();

        // Simulate channel with emoji suffix in its name
        mem.store(
            "cache:channel_name:ch123",
            "cman-kk5🚚",
            MemoryCategory::Custom("channel_cache".to_string()),
            Some("ch123"),
        )
        .await
        .unwrap();
        mem.store(
            "discord_001",
            "@user1 in #cman-kk5🚚 at 2025-01-01T00:00:00Z: loading test message",
            MemoryCategory::Custom("discord".to_string()),
            Some("ch123"),
        )
        .await
        .unwrap();

        let tool = DiscordSearchTool::new(mem);
        // Search without emoji — should still find the message via cache lookup
        let result = tool
            .execute(json!({"channel_name": "cman-kk5"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            result.output.contains("loading test message"),
            "should find message via channel_cache lookup: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn search_channel_name_matches_multiple_channels() {
        let (_tmp, mem) = seeded_discord_mem();

        for (id, suffix, msg) in [
            ("ch1", "cman-kk5🚚", "msg from loading"),
            ("ch2", "cman-kk5-office", "msg from office"),
            ("ch3", "other-channel", "msg from other"),
        ] {
            mem.store(
                &format!("cache:channel_name:{id}"),
                suffix,
                MemoryCategory::Custom("channel_cache".to_string()),
                Some(id),
            )
            .await
            .unwrap();
            mem.store(
                &format!("discord_{id}"),
                &format!("@user in #{suffix} at 2025-01-01T00:00:00Z: {msg}"),
                MemoryCategory::Custom("discord".to_string()),
                Some(id),
            )
            .await
            .unwrap();
        }

        let tool = DiscordSearchTool::new(mem);
        let result = tool
            .execute(json!({"channel_name": "cman-kk5", "limit": 10}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            result.output.contains("msg from loading"),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("msg from office"),
            "{}",
            result.output
        );
        assert!(
            !result.output.contains("msg from other"),
            "{}",
            result.output
        );
    }
}
