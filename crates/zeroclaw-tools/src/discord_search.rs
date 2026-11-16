use async_trait::async_trait;
use rusqlite::{Connection, OpenFlags, params_from_iter};
use serde_json::json;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};

/// Search Discord message history stored read-only in `discord.db`.
pub struct DiscordSearchTool {
    db_path: PathBuf,
}

impl DiscordSearchTool {
    /// Construct from the install-wide `data_dir` (expects `memory/discord.db`).
    pub fn from_data_dir(data_dir: &Path) -> Self {
        Self {
            db_path: data_dir.join("memory").join("discord.db"),
        }
    }

    /// Construct directly from an explicit `discord.db` path.
    pub fn from_db_path(db_path: PathBuf) -> Self {
        Self { db_path }
    }
}

fn search_discord_db(
    db_path: &Path,
    query: &str,
    channel_id: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<String>> {
    if limit == 0 || !db_path.exists() {
        return Ok(Vec::new());
    }

    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    let has_table: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memories')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(Vec::new());
    }

    let mut clauses = Vec::new();
    let mut bind_values: Vec<rusqlite::types::Value> = Vec::new();

    if let Some(ch) = channel_id.map(str::trim).filter(|s| !s.is_empty()) {
        clauses.push("session_id = ?".to_string());
        bind_values.push(rusqlite::types::Value::Text(ch.to_string()));
    }

    let tokens: Vec<&str> = query
        .split_whitespace()
        .filter(|tok| !tok.is_empty() && *tok != "*")
        .collect();
    for tok in tokens {
        let escaped = tok
            .to_lowercase()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        clauses.push("LOWER(content) LIKE ? ESCAPE '\\'".to_string());
        bind_values.push(rusqlite::types::Value::Text(format!("%{escaped}%")));
    }

    let since_dt = since
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()?;
    let until_dt = until
        .map(chrono::DateTime::parse_from_rfc3339)
        .transpose()?;

    let capped_limit = limit.min(500);
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    let limit_sql = if since_dt.is_none() && until_dt.is_none() {
        bind_values.push(rusqlite::types::Value::Integer(
            i64::try_from(capped_limit).unwrap_or(500),
        ));
        " LIMIT ?"
    } else {
        // Time-range filtering parses RFC 3339 offsets in Rust; do not bound the SQL
        // cursor before applying `since`/`until`, or newer rows will hide older hits.
        ""
    };

    let sql = format!(
        "SELECT content, created_at FROM memories{where_sql} ORDER BY updated_at DESC, created_at DESC{limit_sql}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(bind_values.iter()))?;
    let mut matches = Vec::new();
    while let Some(row) = rows.next()? {
        let content: String = row.get(0)?;
        let created_at: String = row.get(1)?;
        if since_dt.is_some() || until_dt.is_some() {
            let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&created_at) else {
                continue;
            };
            if let Some(s) = since_dt
                && ts < s
            {
                continue;
            }
            if let Some(u) = until_dt
                && ts > u
            {
                continue;
            }
        }
        matches.push(content);
        if matches.len() >= capped_limit {
            break;
        }
    }
    Ok(matches)
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
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let channel_id = args
            .get("channel_id")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        let since = args
            .get("since")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        let until = args
            .get("until")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);

        if query.trim().is_empty() && since.is_none() && until.is_none() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(
                    "Provide at least 'query' (keywords) or time range ('since'/'until')".into(),
                ),
            });
        }

        if let Some(ref s) = since
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
        if let Some(ref u) = until
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
        if let (Some(s), Some(u)) = (since.as_deref(), until.as_deref())
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

        let db_path = self.db_path.clone();
        let search_res = tokio::task::spawn_blocking(move || {
            search_discord_db(
                &db_path,
                &query,
                channel_id.as_deref(),
                since.as_deref(),
                until.as_deref(),
                limit,
            )
        })
        .await;

        match search_res {
            Ok(Ok(entries)) if entries.is_empty() => Ok(ToolResult {
                success: true,
                output: "No Discord messages found.".into(),
                error: None,
            }),
            Ok(Ok(entries)) => {
                let mut output = format!("Found {} Discord messages:\n", entries.len());
                for content in &entries {
                    let _ = writeln!(output, "- {content}");
                }
                Ok(ToolResult {
                    success: true,
                    output: output.into(),
                    error: None,
                })
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Discord search failed: {e}")),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!("Discord search task failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn seeded_discord_dir(rows: &[(&str, &str, &str, &str, Option<&str>)]) -> TempDir {
        let tmp = TempDir::new().unwrap();
        let mem_dir = tmp.path().join("memory");
        std::fs::create_dir_all(&mem_dir).unwrap();
        let conn = Connection::open(mem_dir.join("discord.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                 id TEXT PRIMARY KEY,
                 key TEXT NOT NULL,
                 content TEXT NOT NULL,
                 category TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 session_id TEXT
             );",
        )
        .unwrap();
        for (id, key, content, ts, session_id) in rows {
            conn.execute(
                "INSERT INTO memories (id, key, content, category, created_at, updated_at, session_id)
                 VALUES (?1, ?2, ?3, 'discord', ?4, ?4, ?5)",
                rusqlite::params![id, key, content, ts, session_id],
            )
            .unwrap();
        }
        tmp
    }

    #[tokio::test]
    async fn search_empty_or_missing_db_does_not_create_file() {
        let tmp = TempDir::new().unwrap();
        let tool = DiscordSearchTool::from_data_dir(tmp.path());
        let result = tool.execute(json!({"query": "hello"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No Discord messages found"));
        assert!(!tmp.path().join("memory").exists());
    }

    #[tokio::test]
    async fn search_finds_match() {
        let tmp = seeded_discord_dir(&[(
            "m1",
            "discord_001",
            "@user1 in #general at 2025-01-01T00:00:00Z: hello world",
            "2025-01-01T00:00:00Z",
            Some("general"),
        )]);

        let tool = DiscordSearchTool::from_data_dir(tmp.path());
        let result = tool.execute(json!({"query": "hello"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("hello"));
    }

    #[tokio::test]
    async fn search_respects_channel_and_time_filters() {
        let tmp = seeded_discord_dir(&[
            (
                "m1",
                "k1",
                "alpha message",
                "2025-01-01T00:00:00Z",
                Some("ch-1"),
            ),
            (
                "m2",
                "k2",
                "alpha wrong channel",
                "2025-01-02T00:00:00Z",
                Some("ch-2"),
            ),
        ]);

        let tool = DiscordSearchTool::from_data_dir(tmp.path());
        let result = tool
            .execute(json!({
                "query": "alpha",
                "channel_id": "ch-1",
                "since": "2024-12-31T00:00:00Z",
                "until": "2025-01-01T12:00:00Z",
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("alpha message"));
        assert!(!result.output.contains("wrong channel"));
    }

    #[tokio::test]
    async fn search_until_finds_historical_messages_past_newer_rows() {
        struct SeedRow {
            id: String,
            key: String,
            content: String,
            ts: String,
            ch: Option<String>,
        }

        let mut owned_rows = vec![SeedRow {
            id: "m-old".into(),
            key: "k-old".into(),
            content: "historical target message".into(),
            ts: "2025-01-01T12:00:00Z".into(),
            ch: Some("ch-1".into()),
        }];
        for i in 0..45 {
            owned_rows.push(SeedRow {
                id: format!("m-new-{i:02}"),
                key: format!("k-new-{i:02}"),
                content: format!("newer noise {i}"),
                ts: format!("2025-01-03T00:00:{i:02}Z"),
                ch: Some("ch-1".into()),
            });
        }
        let refs: Vec<(&str, &str, &str, &str, Option<&str>)> = owned_rows
            .iter()
            .map(|r| {
                (
                    r.id.as_str(),
                    r.key.as_str(),
                    r.content.as_str(),
                    r.ts.as_str(),
                    r.ch.as_deref(),
                )
            })
            .collect();
        let tmp = seeded_discord_dir(&refs);

        let tool = DiscordSearchTool::from_data_dir(tmp.path());
        let result = tool
            .execute(json!({
                "until": "2025-01-02T00:00:00Z",
                "limit": 10,
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            result.output.contains("historical target message"),
            "got: {}",
            result.output
        );
        assert!(!result.output.contains("newer noise"));
    }

    #[tokio::test]
    async fn search_requires_query_or_time() {
        let tmp = TempDir::new().unwrap();
        let tool = DiscordSearchTool::from_data_dir(tmp.path());
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("at least"));
    }

    #[test]
    fn name_and_schema() {
        let tmp = TempDir::new().unwrap();
        let tool = DiscordSearchTool::from_data_dir(tmp.path());
        assert_eq!(tool.name(), "discord_search");
        assert!(tool.parameters_schema()["properties"]["query"].is_object());
    }
}
