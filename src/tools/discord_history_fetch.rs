use super::channel_runtime_context::current_channel_runtime_context;
use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;

const DEFAULT_LIMIT: i64 = 50;
const MIN_LIMIT: i64 = 1;
const MAX_LIMIT: i64 = 100;
const DEFAULT_DISCORD_API_BASE: &str = "https://discord.com/api/v10";

pub struct DiscordHistoryFetchTool {
    security: Arc<SecurityPolicy>,
    bot_token: String,
    api_base_url: String,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct DiscordApiMessage {
    #[serde(default)]
    id: String,
    #[serde(default)]
    timestamp: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    attachments: Vec<DiscordApiAttachment>,
    #[serde(default)]
    author: DiscordApiAuthor,
    #[serde(default, rename = "type")]
    message_type: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct DiscordApiAuthor {
    #[serde(default)]
    id: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    global_name: Option<String>,
    #[serde(default)]
    bot: bool,
}

#[derive(Debug, Deserialize)]
struct DiscordApiAttachment {
    #[serde(default)]
    id: String,
    #[serde(default)]
    filename: String,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Serialize)]
struct HistoryFetchOutput {
    channel_id: String,
    fetched_count: usize,
    unique_human_authors: usize,
    messages: Vec<HistoryMessage>,
}

#[derive(Debug, Serialize)]
struct HistoryMessage {
    id: String,
    timestamp: String,
    author: HistoryAuthor,
    content: String,
    attachments: Vec<HistoryAttachment>,
}

#[derive(Debug, Serialize)]
struct HistoryAuthor {
    id: String,
    username: String,
    display_name: String,
    is_bot: bool,
}

#[derive(Debug, Serialize)]
struct HistoryAttachment {
    id: String,
    filename: String,
    content_type: Option<String>,
    size: Option<u64>,
    url: Option<String>,
}

impl DiscordHistoryFetchTool {
    pub fn new(security: Arc<SecurityPolicy>, bot_token: String) -> Self {
        Self {
            security,
            bot_token,
            api_base_url: DEFAULT_DISCORD_API_BASE.to_string(),
            client: crate::config::build_runtime_proxy_client_with_timeouts(
                "tool.discord_history_fetch",
                30,
                10,
            ),
        }
    }

    #[cfg(test)]
    fn new_with_base_url(
        security: Arc<SecurityPolicy>,
        bot_token: String,
        api_base_url: String,
    ) -> Self {
        Self {
            security,
            bot_token,
            api_base_url,
            client: reqwest::Client::new(),
        }
    }

    fn error_result(message: impl Into<String>) -> ToolResult {
        ToolResult {
            success: false,
            output: String::new(),
            error: Some(message.into()),
        }
    }

    fn trim_opt_string(value: Option<&str>) -> Option<String> {
        value
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    }

    fn parse_bool(args: &serde_json::Value, key: &str, default: bool) -> bool {
        args.get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(default)
    }

    fn parse_limit(args: &serde_json::Value) -> i64 {
        let raw = args
            .get("limit")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(DEFAULT_LIMIT);
        raw.clamp(MIN_LIMIT, MAX_LIMIT)
    }

    fn parse_cursor_args(
        args: &serde_json::Value,
    ) -> anyhow::Result<(Option<String>, Option<String>, Option<String>)> {
        let before = Self::trim_opt_string(args.get("before_message_id").and_then(|v| v.as_str()));
        let after = Self::trim_opt_string(args.get("after_message_id").and_then(|v| v.as_str()));
        let around = Self::trim_opt_string(args.get("around_message_id").and_then(|v| v.as_str()));

        let set_count = [before.is_some(), after.is_some(), around.is_some()]
            .into_iter()
            .filter(|v| *v)
            .count();
        if set_count > 1 {
            anyhow::bail!(
                "Only one of before_message_id, after_message_id, or around_message_id may be set"
            );
        }

        Ok((before, after, around))
    }

    fn resolve_channel_id(
        &self,
        args: &serde_json::Value,
        allow_cross_channel: bool,
    ) -> anyhow::Result<String> {
        let explicit_channel_id =
            Self::trim_opt_string(args.get("channel_id").and_then(|v| v.as_str()));
        let context = current_channel_runtime_context();

        match (context, explicit_channel_id) {
            (Some(ctx), Some(channel_id)) => {
                if ctx.channel == "discord"
                    && !allow_cross_channel
                    && !ctx.reply_target.is_empty()
                    && channel_id != ctx.reply_target
                {
                    anyhow::bail!(
                        "Cross-channel fetch blocked: requested channel_id differs from current Discord conversation. Set allow_cross_channel=true to override."
                    );
                }
                Ok(channel_id)
            }
            (Some(ctx), None) if ctx.channel == "discord" => {
                let reply_target = ctx.reply_target.trim();
                if reply_target.is_empty() {
                    anyhow::bail!(
                        "Current Discord runtime context has an empty reply_target; pass channel_id explicitly"
                    );
                }
                Ok(reply_target.to_string())
            }
            (Some(ctx), None) => anyhow::bail!(
                "channel_id is required outside Discord runtime context (current channel={})",
                ctx.channel
            ),
            (None, Some(channel_id)) => Ok(channel_id),
            (None, None) => {
                anyhow::bail!("channel_id is required when no Discord runtime context is available")
            }
        }
    }

    fn is_system_message(message_type: Option<u64>) -> bool {
        // Discord type 0 is a regular chat message. Non-zero types include
        // system/service-style messages and non-standard events.
        message_type.unwrap_or(0) != 0
    }

    fn message_url(&self, channel_id: &str) -> String {
        format!(
            "{}/channels/{channel_id}/messages",
            self.api_base_url.trim_end_matches('/')
        )
    }

    fn display_name(author: &DiscordApiAuthor) -> String {
        author
            .global_name
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| author.username.clone())
    }

    fn render_history_output(
        &self,
        channel_id: String,
        mut messages: Vec<DiscordApiMessage>,
        include_bots: bool,
        include_system: bool,
        include_content: bool,
        include_attachments: bool,
    ) -> anyhow::Result<ToolResult> {
        // Discord API returns newest-first; return oldest-first for predictable
        // downstream reasoning and sampling.
        messages.reverse();

        let mut human_authors = HashSet::new();
        let mut rendered_messages = Vec::new();

        for msg in messages {
            if !include_bots && msg.author.bot {
                continue;
            }
            if !include_system && Self::is_system_message(msg.message_type) {
                continue;
            }

            if !msg.author.bot && !msg.author.id.trim().is_empty() {
                human_authors.insert(msg.author.id.clone());
            }

            let display_name = Self::display_name(&msg.author);
            let author = HistoryAuthor {
                id: msg.author.id.clone(),
                username: msg.author.username.clone(),
                display_name,
                is_bot: msg.author.bot,
            };

            let attachments = if include_attachments {
                msg.attachments
                    .into_iter()
                    .map(|att| HistoryAttachment {
                        id: att.id,
                        filename: att.filename,
                        content_type: att.content_type,
                        size: att.size,
                        url: att.url,
                    })
                    .collect()
            } else {
                Vec::new()
            };

            rendered_messages.push(HistoryMessage {
                id: msg.id,
                timestamp: msg.timestamp,
                author,
                content: if include_content {
                    msg.content
                } else {
                    String::new()
                },
                attachments,
            });
        }

        let output = HistoryFetchOutput {
            channel_id,
            fetched_count: rendered_messages.len(),
            unique_human_authors: human_authors.len(),
            messages: rendered_messages,
        };

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }
}

#[async_trait]
impl Tool for DiscordHistoryFetchTool {
    fn name(&self) -> &str {
        "discord_history_fetch"
    }

    fn description(&self) -> &str {
        "Fetch Discord channel message history on demand. In Discord runtime it auto-targets the current conversation by default; outside Discord runtime pass channel_id explicitly."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "channel_id": {
                    "type": "string",
                    "description": "Discord channel ID. Optional in Discord runtime (auto-resolves to current conversation); required elsewhere."
                },
                "limit": {
                    "type": "integer",
                    "description": "Number of messages to fetch (clamped to 1..100). Default: 50.",
                    "default": 50
                },
                "before_message_id": {
                    "type": "string",
                    "description": "Fetch messages before this message ID."
                },
                "after_message_id": {
                    "type": "string",
                    "description": "Fetch messages after this message ID."
                },
                "around_message_id": {
                    "type": "string",
                    "description": "Fetch messages around this message ID."
                },
                "include_bots": {
                    "type": "boolean",
                    "description": "Include bot-authored messages. Default: false.",
                    "default": false
                },
                "include_system": {
                    "type": "boolean",
                    "description": "Include non-standard/system Discord message types. Default: false.",
                    "default": false
                },
                "include_content": {
                    "type": "boolean",
                    "description": "Include message content. Default: true.",
                    "default": true
                },
                "include_attachments": {
                    "type": "boolean",
                    "description": "Include attachment metadata. Default: true.",
                    "default": true
                },
                "allow_cross_channel": {
                    "type": "boolean",
                    "description": "When in Discord runtime, allow explicit channel_id different from current conversation. Default: false.",
                    "default": false
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if !self.security.can_act() {
            return Ok(Self::error_result("Action blocked: autonomy is read-only"));
        }
        if !self.security.record_action() {
            return Ok(Self::error_result("Action blocked: rate limit exceeded"));
        }

        let token = self.bot_token.trim();
        if token.is_empty() {
            return Ok(Self::error_result(
                "Discord history fetch requires channels_config.discord.bot_token",
            ));
        }

        let allow_cross_channel = Self::parse_bool(&args, "allow_cross_channel", false);
        let channel_id = match self.resolve_channel_id(&args, allow_cross_channel) {
            Ok(v) => v,
            Err(e) => return Ok(Self::error_result(e.to_string())),
        };
        let limit = Self::parse_limit(&args);
        let include_bots = Self::parse_bool(&args, "include_bots", false);
        let include_system = Self::parse_bool(&args, "include_system", false);
        let include_content = Self::parse_bool(&args, "include_content", true);
        let include_attachments = Self::parse_bool(&args, "include_attachments", true);
        let (before, after, around) = match Self::parse_cursor_args(&args) {
            Ok(v) => v,
            Err(e) => return Ok(Self::error_result(e.to_string())),
        };

        let mut query: Vec<(String, String)> = vec![("limit".to_string(), limit.to_string())];
        if let Some(v) = before {
            query.push(("before".to_string(), v));
        }
        if let Some(v) = after {
            query.push(("after".to_string(), v));
        }
        if let Some(v) = around {
            query.push(("around".to_string(), v));
        }

        let response = match self
            .client
            .get(self.message_url(&channel_id))
            .header("Authorization", format!("Bot {token}"))
            .query(&query)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => {
                return Ok(Self::error_result(format!(
                    "Discord history request failed: {err}"
                )))
            }
        };

        let status = response.status();
        let retry_after = response
            .headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = response
            .text()
            .await
            .unwrap_or_else(|err| format!("<failed to read response body: {err}>"));

        if status.as_u16() == 429 {
            let suffix = if retry_after.is_empty() {
                String::new()
            } else {
                format!(" Retry-After: {retry_after}.")
            };
            return Ok(Self::error_result(format!(
                "Discord API rate limited (429).{suffix} Response: {body}"
            )));
        }

        if !status.is_success() {
            return Ok(Self::error_result(format!(
                "Discord history fetch failed ({}): {body}",
                status
            )));
        }

        let messages: Vec<DiscordApiMessage> = match serde_json::from_str(&body) {
            Ok(parsed) => parsed,
            Err(err) => {
                return Ok(Self::error_result(format!(
                    "Discord history fetch returned invalid JSON: {err}"
                )))
            }
        };

        self.render_history_output(
            channel_id,
            messages,
            include_bots,
            include_system,
            include_content,
            include_attachments,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::channel_runtime_context::{
        with_channel_runtime_context, ChannelRuntimeContext,
    };
    use serde_json::Value;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_context(channel: &str, reply_target: &str) -> ChannelRuntimeContext {
        ChannelRuntimeContext {
            channel: channel.to_string(),
            reply_target: reply_target.to_string(),
            thread_ts: None,
            sender: "user_a".to_string(),
            message_id: "discord_123".to_string(),
        }
    }

    fn test_tool(base_url: String) -> DiscordHistoryFetchTool {
        DiscordHistoryFetchTool::new_with_base_url(
            Arc::new(SecurityPolicy::default()),
            "test-token".to_string(),
            base_url,
        )
    }

    async fn execute_with_ctx(
        tool: &DiscordHistoryFetchTool,
        ctx: ChannelRuntimeContext,
        args: serde_json::Value,
    ) -> ToolResult {
        with_channel_runtime_context(ctx, async { tool.execute(args).await.unwrap() }).await
    }

    #[tokio::test]
    async fn resolve_requires_channel_id_without_context() {
        let tool = test_tool("http://localhost".to_string());
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("channel_id is required"),
            "unexpected error message"
        );
    }

    #[tokio::test]
    async fn resolve_requires_channel_id_outside_discord_context() {
        let tool = test_tool("http://localhost".to_string());
        let result = execute_with_ctx(&tool, test_context("telegram", "chat_1"), json!({})).await;
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("required outside Discord runtime context"),
            "unexpected error message"
        );
    }

    #[tokio::test]
    async fn resolve_uses_current_discord_reply_target_when_channel_id_omitted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/channels/C456/messages"))
            .and(query_param("limit", "50"))
            .and(header("authorization", "Bot test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;

        let tool = test_tool(server.uri());
        let result = execute_with_ctx(&tool, test_context("discord", "C456"), json!({})).await;
        assert!(result.success);

        let parsed: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["channel_id"], "C456");
        assert_eq!(parsed["fetched_count"], 0);
    }

    #[tokio::test]
    async fn resolve_blocks_cross_channel_by_default() {
        let tool = test_tool("http://localhost".to_string());
        let result = execute_with_ctx(
            &tool,
            test_context("discord", "C456"),
            json!({"channel_id": "C123"}),
        )
        .await;
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap_or_default()
                .contains("Cross-channel fetch blocked"),
            "unexpected error message"
        );
    }

    #[tokio::test]
    async fn resolve_allows_cross_channel_with_opt_in() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/channels/C123/messages"))
            .and(query_param("limit", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;

        let tool = test_tool(server.uri());
        let result = execute_with_ctx(
            &tool,
            test_context("discord", "C456"),
            json!({"channel_id": "C123", "allow_cross_channel": true}),
        )
        .await;

        assert!(result.success);
        let parsed: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["channel_id"], "C123");
    }

    #[tokio::test]
    async fn successful_fetch_is_oldest_first_and_filters_default_bot_system() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/channels/C123/messages"))
            .and(query_param("limit", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "id": "3",
                    "timestamp": "2026-02-25T03:00:00.000000+00:00",
                    "content": "bot message",
                    "type": 0,
                    "author": {"id": "bot_1", "username": "bot", "bot": true},
                    "attachments": []
                },
                {
                    "id": "2",
                    "timestamp": "2026-02-25T02:00:00.000000+00:00",
                    "content": "system message",
                    "type": 1,
                    "author": {"id": "u_2", "username": "user2", "bot": false},
                    "attachments": []
                },
                {
                    "id": "1",
                    "timestamp": "2026-02-25T01:00:00.000000+00:00",
                    "content": "hello",
                    "type": 0,
                    "author": {"id": "u_1", "username": "user1", "global_name": "User One", "bot": false},
                    "attachments": [{
                        "id": "a1",
                        "filename": "file.txt",
                        "content_type": "text/plain",
                        "size": 12,
                        "url": "https://cdn.discordapp.com/file.txt"
                    }]
                }
            ])))
            .mount(&server)
            .await;

        let tool = test_tool(server.uri());
        let result = tool.execute(json!({"channel_id": "C123"})).await.unwrap();
        assert!(result.success);

        let parsed: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["fetched_count"], 1);
        assert_eq!(parsed["unique_human_authors"], 1);
        assert_eq!(parsed["messages"][0]["id"], "1");
        assert_eq!(parsed["messages"][0]["author"]["display_name"], "User One");
        assert_eq!(parsed["messages"][0]["attachments"][0]["id"], "a1");
    }

    #[tokio::test]
    async fn include_flags_keep_messages_but_strip_content_and_attachments_when_disabled() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/channels/C777/messages"))
            .and(query_param("limit", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {
                    "id": "2",
                    "timestamp": "2026-02-25T02:00:00.000000+00:00",
                    "content": "bot message",
                    "type": 0,
                    "author": {"id": "bot_1", "username": "bot", "bot": true},
                    "attachments": [{"id": "a2", "filename": "f.txt", "url": "https://cdn.example/f.txt"}]
                },
                {
                    "id": "1",
                    "timestamp": "2026-02-25T01:00:00.000000+00:00",
                    "content": "system message",
                    "type": 1,
                    "author": {"id": "u_1", "username": "user1", "bot": false},
                    "attachments": [{"id": "a1", "filename": "g.txt", "url": "https://cdn.example/g.txt"}]
                }
            ])))
            .mount(&server)
            .await;

        let tool = test_tool(server.uri());
        let result = tool
            .execute(json!({
                "channel_id": "C777",
                "include_bots": true,
                "include_system": true,
                "include_content": false,
                "include_attachments": false
            }))
            .await
            .unwrap();
        assert!(result.success);

        let parsed: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["fetched_count"], 2);
        assert_eq!(parsed["messages"][0]["id"], "1");
        assert_eq!(parsed["messages"][1]["id"], "2");
        assert_eq!(parsed["messages"][0]["content"], "");
        assert_eq!(
            parsed["messages"][0]["attachments"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn limit_is_clamped_to_discord_max() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/channels/C123/messages"))
            .and(query_param("limit", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;

        let tool = test_tool(server.uri());
        let result = tool
            .execute(json!({"channel_id": "C123", "limit": 999}))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn returns_actionable_error_for_rate_limit() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/channels/C999/messages"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "2")
                    .set_body_string("{\"message\":\"Too Many Requests\"}"),
            )
            .mount(&server)
            .await;

        let tool = test_tool(server.uri());
        let result = tool.execute(json!({"channel_id": "C999"})).await.unwrap();
        assert!(!result.success);
        let err = result.error.unwrap_or_default();
        assert!(err.contains("rate limited (429)"));
        assert!(err.contains("Retry-After: 2"));
    }

    #[tokio::test]
    async fn returns_actionable_error_for_non_success_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/channels/C403/messages"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let tool = test_tool(server.uri());
        let result = tool.execute(json!({"channel_id": "C403"})).await.unwrap();
        assert!(!result.success);
        let err = result.error.unwrap_or_default();
        assert!(err.contains("Discord history fetch failed (403 Forbidden)"));
        assert!(err.contains("forbidden"));
    }
}
