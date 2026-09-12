use async_trait::async_trait;
use serde_json::json;
use std::collections::BTreeSet;
use std::fmt::Write;
use std::sync::{Arc, OnceLock};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::{SecurityPolicy, ToolOperation};
use zeroclaw_memory::{Memory, SqliteMemory};

/// Rows archived before per-channel provenance stamping carry the memory
/// schema's namespace column default instead of a `discord.<alias>` ref.
const LEGACY_NAMESPACE: &str = "default";
const TOOL_DESCRIPTION_KEY: &str = "tool-discord-search";
static TOOL_DESCRIPTION: OnceLock<String> = OnceLock::new();

/// Archive rows the calling agent may read, bound at tool construction
/// from trusted config (never from model-supplied arguments).
///
/// The Discord archive (`discord.db`) is shared across every configured
/// Discord alias; each row's `namespace` records the ChannelRef
/// (`discord.<alias>`) of the channel that archived it. An agent reads
/// rows whose namespace is one of its owned channel refs, plus
/// unattributed legacy rows when the install has at most one enabled
/// agent (a sole agent owns everything; in multi-agent installs legacy
/// rows fail closed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordArchiveScope {
    owned_channel_refs: BTreeSet<String>,
    include_unattributed: bool,
}

impl DiscordArchiveScope {
    pub fn new<I, S>(owned_channel_refs: I, include_unattributed: bool) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            owned_channel_refs: owned_channel_refs.into_iter().map(Into::into).collect(),
            include_unattributed,
        }
    }

    fn allowed_namespaces(&self) -> Vec<String> {
        let mut namespaces: Vec<String> = self.owned_channel_refs.iter().cloned().collect();
        if self.include_unattributed {
            namespaces.push(LEGACY_NAMESPACE.to_string());
        }
        namespaces
    }
}

/// Search Discord message history stored in discord.db.
pub struct DiscordSearchTool {
    discord_memory: Arc<SqliteMemory>,
    security: Arc<SecurityPolicy>,
    archive_scope: Option<DiscordArchiveScope>,
}

impl DiscordSearchTool {
    pub fn new(discord_memory: Arc<SqliteMemory>, security: Arc<SecurityPolicy>) -> Self {
        Self {
            discord_memory,
            security,
            archive_scope: None,
        }
    }

    pub fn for_agent(
        discord_memory: Arc<SqliteMemory>,
        security: Arc<SecurityPolicy>,
        archive_scope: DiscordArchiveScope,
    ) -> Self {
        Self {
            discord_memory,
            security,
            archive_scope: Some(archive_scope),
        }
    }
}

#[async_trait]
impl Tool for DiscordSearchTool {
    fn name(&self) -> &str {
        "discord_search"
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
            .get_or_init(|| crate::i18n::get_required_tool_string(TOOL_DESCRIPTION_KEY))
            .as_str()
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
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, "discord_search")
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(error),
            });
        }

        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let channel_id = args.get("channel_id").and_then(|v| v.as_str());
        let since = args.get("since").and_then(|v| v.as_str());
        let until = args.get("until").and_then(|v| v.as_str());

        if query.trim().is_empty() && since.is_none() && until.is_none() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(
                    "Provide at least 'query' (keywords) or time range ('since'/'until')".into(),
                ),
            });
        }

        if let Some(s) = since
            && chrono::DateTime::parse_from_rfc3339(s).is_err()
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Invalid 'since' date: {s}. Expected RFC 3339, e.g. 2025-03-01T00:00:00Z"
                )),
            });
        }
        if let Some(u) = until
            && chrono::DateTime::parse_from_rfc3339(u).is_err()
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Invalid 'until' date: {u}. Expected RFC 3339, e.g. 2025-03-01T00:00:00Z"
                )),
            });
        }
        if let (Some(s), Some(u)) = (since, until)
            && let (Ok(s_dt), Ok(u_dt)) = (
                chrono::DateTime::parse_from_rfc3339(s),
                chrono::DateTime::parse_from_rfc3339(u),
            )
            && s_dt >= u_dt
        {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some("'since' must be before 'until'".into()),
            });
        }

        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(10, |v| v as usize);

        let allowed_namespaces = self
            .archive_scope
            .as_ref()
            .map(DiscordArchiveScope::allowed_namespaces);
        let recalled = if let Some(allowed_namespaces) = allowed_namespaces.as_deref() {
            self.discord_memory
                .recall_in_namespaces(allowed_namespaces, query, limit, channel_id, since, until)
                .await
        } else {
            self.discord_memory
                .recall(query, limit, channel_id, since, until)
                .await
        };

        match recalled {
            Ok(entries) => {
                if entries.is_empty() {
                    return Ok(ToolResult {
                        success: true,
                        output: "No Discord messages found.".into(),
                        error: None,
                    });
                }
                let mut output = format!("Found {} Discord messages:\n", entries.len());
                for entry in &entries {
                    let score = entry
                        .score
                        .map_or_else(String::new, |s| format!(" [{:.0}%]", s * 100.0));
                    let _ = writeln!(output, "- {}{score}", entry.content);
                }
                Ok(ToolResult {
                    success: true,
                    output: output.into(),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Discord search failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_memory::{MemoryCategory, SqliteMemory};

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn seeded_discord_mem() -> (TempDir, Arc<SqliteMemory>) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new_named("test", tmp.path(), "discord").unwrap();
        (tmp, Arc::new(mem))
    }

    /// Archive a row the way DiscordChannel does post-provenance: the
    /// numeric Discord channel id as session, the archiving channel's
    /// ChannelRef as namespace.
    async fn archive_row(
        mem: &Arc<SqliteMemory>,
        key: &str,
        content: &str,
        discord_channel_id: &str,
        namespace: Option<&str>,
    ) {
        mem.store_with_metadata(
            key,
            content,
            MemoryCategory::Custom("discord".to_string()),
            Some(discord_channel_id),
            namespace,
            None,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn search_empty() {
        let (_tmp, mem) = seeded_discord_mem();
        let tool = DiscordSearchTool::new(mem, test_security());
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

        let tool = DiscordSearchTool::new(mem, test_security());
        let result = tool.execute(json!({"query": "hello"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("hello"));
    }

    #[tokio::test]
    async fn search_requires_query_or_time() {
        let (_tmp, mem) = seeded_discord_mem();
        let tool = DiscordSearchTool::new(mem, test_security());
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("at least"));
    }

    #[tokio::test]
    async fn scoped_search_excludes_foreign_alias_rows() {
        let (_tmp, mem) = seeded_discord_mem();
        archive_row(
            &mem,
            "discord_own",
            "@a in #111 at 2025-01-01T00:00:00Z: shared keyword own",
            "111",
            Some("discord.mine"),
        )
        .await;
        archive_row(
            &mem,
            "discord_foreign",
            "@b in #222 at 2025-01-01T00:00:00Z: shared keyword foreign",
            "222",
            Some("discord.theirs"),
        )
        .await;

        let tool = DiscordSearchTool::for_agent(
            mem,
            test_security(),
            DiscordArchiveScope::new(["discord.mine"], false),
        );

        let result = tool
            .execute(json!({"query": "shared keyword"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("own"));
        assert!(!result.output.contains("foreign"));
    }

    #[tokio::test]
    async fn scoped_search_ignores_model_supplied_channel_id_as_authorization() {
        let (_tmp, mem) = seeded_discord_mem();
        archive_row(
            &mem,
            "discord_foreign",
            "@b in #222 at 2025-01-01T00:00:00Z: private foreign text",
            "222",
            Some("discord.theirs"),
        )
        .await;

        let tool = DiscordSearchTool::for_agent(
            mem,
            test_security(),
            DiscordArchiveScope::new(["discord.mine"], false),
        );

        // The model names the foreign channel id explicitly; the arg is a
        // selector, not an authorization, so nothing may come back.
        let result = tool
            .execute(json!({"query": "private", "channel_id": "222"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("No Discord messages found"));
        assert!(!result.output.contains("private foreign text"));
    }

    #[tokio::test]
    async fn scoped_search_legacy_rows_fail_closed_in_multi_agent_mode() {
        let (_tmp, mem) = seeded_discord_mem();
        // Pre-provenance archive row: bare store, namespace stays 'default'.
        mem.store(
            "discord_legacy",
            "@old in #333 at 2024-01-01T00:00:00Z: legacy text",
            MemoryCategory::Custom("discord".to_string()),
            Some("333"),
        )
        .await
        .unwrap();

        let tool = DiscordSearchTool::for_agent(
            mem,
            test_security(),
            DiscordArchiveScope::new(["discord.mine"], false),
        );

        let result = tool.execute(json!({"query": "legacy"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No Discord messages found"));
    }

    #[tokio::test]
    async fn scoped_search_legacy_rows_visible_in_single_agent_mode() {
        let (_tmp, mem) = seeded_discord_mem();
        mem.store(
            "discord_legacy",
            "@old in #333 at 2024-01-01T00:00:00Z: legacy text",
            MemoryCategory::Custom("discord".to_string()),
            Some("333"),
        )
        .await
        .unwrap();

        let tool = DiscordSearchTool::for_agent(
            mem,
            test_security(),
            DiscordArchiveScope::new(["discord.mine"], true),
        );

        let result = tool.execute(json!({"query": "legacy"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("legacy text"));
    }

    #[tokio::test]
    async fn scoped_search_filters_before_limit() {
        let (_tmp, mem) = seeded_discord_mem();
        archive_row(
            &mem,
            "discord_own",
            "@a in #111 at 2025-01-01T00:00:00Z: owned archive row",
            "111",
            Some("discord.mine"),
        )
        .await;
        for i in 0..201 {
            archive_row(
                &mem,
                &format!("discord_foreign_{i}"),
                &format!("@b in #222 at 2025-01-02T00:00:00Z: newer foreign row {i}"),
                "222",
                Some("discord.theirs"),
            )
            .await;
        }

        let tool = DiscordSearchTool::for_agent(
            mem,
            test_security(),
            DiscordArchiveScope::new(["discord.mine"], false),
        );

        // More than the old 200-row over-fetch cap rank ahead of the owned
        // row by recency. Query-boundary namespace scoping must still return
        // the owned row at limit 1.
        let result = tool
            .execute(json!({"since": "2000-01-01T00:00:00Z", "limit": 1}))
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);
        assert!(
            result.output.contains("owned archive row"),
            "{}",
            result.output
        );
        assert!(!result.output.contains("foreign"));
    }

    #[test]
    fn score_formatted_as_percent() {
        let score: Option<f64> = Some(0.63);
        let formatted = score.map_or_else(String::new, |s| format!(" [{:.0}%]", s * 100.0));
        assert_eq!(formatted, " [63%]");

        let score: Option<f64> = Some(0.42);
        let formatted = score.map_or_else(String::new, |s| format!(" [{:.0}%]", s * 100.0));
        assert_eq!(formatted, " [42%]");

        let score: Option<f64> = None;
        let formatted = score.map_or_else(String::new, |s| format!(" [{:.0}%]", s * 100.0));
        assert_eq!(formatted, "");
    }

    #[test]
    fn name_and_schema() {
        let (_tmp, mem) = seeded_discord_mem();
        let tool = DiscordSearchTool::new(mem, test_security());
        assert_eq!(tool.name(), "discord_search");
        assert!(tool.description().contains("this agent's configured"));
        assert!(tool.parameters_schema()["properties"]["query"].is_object());
    }
}
