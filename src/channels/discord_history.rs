use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::memory::{Memory, MemoryCategory};

/// Discord History channel — connects via Gateway WebSocket, stores ALL non-bot messages
/// to a dedicated discord.db, and forwards @mention messages to the agent.
pub struct DiscordHistoryChannel {
    bot_token: String,
    guild_id: Option<String>,
    allowed_users: Vec<String>,
    /// Channel IDs to watch. Empty = watch all channels.
    channel_ids: Vec<String>,
    /// Dedicated discord.db memory backend.
    discord_memory: Arc<dyn Memory>,
    typing_handles: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    proxy_url: Option<String>,
    /// When false, DM messages are not stored in discord.db.
    store_dms: bool,
    /// When false, @mentions in DMs are not forwarded to the agent.
    respond_to_dms: bool,
}

impl DiscordHistoryChannel {
    pub fn new(
        bot_token: String,
        guild_id: Option<String>,
        allowed_users: Vec<String>,
        channel_ids: Vec<String>,
        discord_memory: Arc<dyn Memory>,
        store_dms: bool,
        respond_to_dms: bool,
    ) -> Self {
        Self {
            bot_token,
            guild_id,
            allowed_users,
            channel_ids,
            discord_memory,
            typing_handles: Mutex::new(HashMap::new()),
            proxy_url: None,
            store_dms,
            respond_to_dms,
        }
    }

    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_channel_proxy_client(
            "channel.discord_history",
            self.proxy_url.as_deref(),
        )
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        if self.allowed_users.is_empty() {
            return true; // default open for logging channel
        }
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    fn is_channel_watched(&self, channel_id: &str) -> bool {
        self.channel_ids.is_empty() || self.channel_ids.iter().any(|c| c == channel_id)
    }

    fn bot_user_id_from_token(token: &str) -> Option<String> {
        let part = token.split('.').next()?;
        base64_decode(part)
    }

    async fn resolve_channel_name(&self, channel_id: &str) -> String {
        // 1. Check persistent database (via discord_memory)
        let cache_key = format!("cache:channel_name:{}", channel_id);

        if let Ok(Some(cached_mem)) = self.discord_memory.get(&cache_key).await {
            // Check if it's still fresh (e.g., less than 24 hours old)
            // Note: cached_mem.timestamp is an RFC3339 string
            let is_fresh =
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&cached_mem.timestamp) {
                    chrono::Utc::now().signed_duration_since(ts.with_timezone(&chrono::Utc))
                        < chrono::Duration::hours(24)
                } else {
                    false
                };

            if is_fresh {
                return cached_mem.content.clone();
            }
        }

        // 2. Fetch from API (either not in DB or stale)
        let url = format!("https://discord.com/api/v10/channels/{channel_id}");
        let resp = self
            .http_client()
            .get(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await;

        let name = if let Ok(r) = resp {
            if let Ok(json) = r.json::<serde_json::Value>().await {
                json.get("name")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        // For DMs, there might not be a 'name', use the recipient's username if available
                        json.get("recipients")
                            .and_then(|r| r.as_array())
                            .and_then(|a| a.first())
                            .and_then(|u| u.get("username"))
                            .and_then(|un| un.as_str())
                            .map(|s| format!("dm-{}", s))
                    })
            } else {
                None
            }
        } else {
            None
        };

        let resolved = name.unwrap_or_else(|| channel_id.to_string());

        // 3. Store in persistent database
        let _ = self
            .discord_memory
            .store(
                &cache_key,
                &resolved,
                crate::memory::MemoryCategory::Custom("channel_cache".to_string()),
                Some(channel_id),
            )
            .await;

        resolved
    }
}

const BASE64_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[allow(clippy::cast_possible_truncation)]
fn base64_decode(input: &str) -> Option<String> {
    let padded = match input.len() % 4 {
        2 => format!("{input}=="),
        3 => format!("{input}="),
        _ => input.to_string(),
    };
    let mut bytes = Vec::new();
    let chars: Vec<u8> = padded.bytes().collect();
    for chunk in chars.chunks(4) {
        if chunk.len() < 4 {
            break;
        }
        let mut v = [0usize; 4];
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                v[i] = 0;
            } else {
                v[i] = BASE64_ALPHABET.iter().position(|&a| a == b)?;
            }
        }
        bytes.push(((v[0] << 2) | (v[1] >> 4)) as u8);
        if chunk[2] != b'=' {
            bytes.push((((v[1] & 0xF) << 4) | (v[2] >> 2)) as u8);
        }
        if chunk[3] != b'=' {
            bytes.push((((v[2] & 0x3) << 6) | v[3]) as u8);
        }
    }
    String::from_utf8(bytes).ok()
}

fn contains_bot_mention(content: &str, bot_user_id: &str) -> bool {
    if bot_user_id.is_empty() {
        return false;
    }
    content.contains(&format!("<@{bot_user_id}>"))
        || content.contains(&format!("<@!{bot_user_id}>"))
}

fn strip_bot_mention(content: &str, bot_user_id: &str) -> String {
    let mut result = content.to_string();
    for tag in [format!("<@{bot_user_id}>"), format!("<@!{bot_user_id}>")] {
        result = result.replace(&tag, " ");
    }
    result.trim().to_string()
}

#[async_trait]
impl Channel for DiscordHistoryChannel {
    fn name(&self) -> &str {
        "discord_history"
    }

    /// Send a reply back to Discord (used when agent responds to @mention).
    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let content = super::strip_tool_call_tags(&message.content);
        let url = format!(
            "https://discord.com/api/v10/channels/{}/messages",
            message.recipient
        );
        self.http_client()
            .post(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .json(&json!({"content": content}))
            .send()
            .await?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let bot_user_id = Self::bot_user_id_from_token(&self.bot_token).unwrap_or_default();

        // Get Gateway URL
        let gw_resp: serde_json::Value = self
            .http_client()
            .get("https://discord.com/api/v10/gateway/bot")
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await?
            .json()
            .await?;

        let gw_url = gw_resp
            .get("url")
            .and_then(|u| u.as_str())
            .unwrap_or("wss://gateway.discord.gg");

        let ws_url = format!("{gw_url}/?v=10&encoding=json");
        tracing::info!("DiscordHistory: connecting to gateway...");

        let (ws_stream, _) = crate::config::ws_connect_with_proxy(
            &ws_url,
            "channel.discord",
            self.proxy_url.as_deref(),
        )
        .await?;
        let (mut write, mut read) = ws_stream.split();

        // Read Hello (opcode 10)
        let hello = read.next().await.ok_or(anyhow::anyhow!("No hello"))??;
        let hello_data: serde_json::Value = serde_json::from_str(&hello.to_string())?;
        let heartbeat_interval = hello_data
            .get("d")
            .and_then(|d| d.get("heartbeat_interval"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(41250);

        // Identify with intents for guild + DM messages + message content
        let identify = json!({
            "op": 2,
            "d": {
                "token": self.bot_token,
                "intents": 37377,
                "properties": {
                    "os": "linux",
                    "browser": "zeroclaw",
                    "device": "zeroclaw"
                }
            }
        });
        write
            .send(Message::Text(identify.to_string().into()))
            .await?;

        tracing::info!("DiscordHistory: connected and identified");

        let mut sequence: i64 = -1;

        let (hb_tx, mut hb_rx) = tokio::sync::mpsc::channel::<()>(1);
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_millis(heartbeat_interval));
            loop {
                interval.tick().await;
                if hb_tx.send(()).await.is_err() {
                    break;
                }
            }
        });

        let guild_filter = self.guild_id.clone();
        let discord_memory = Arc::clone(&self.discord_memory);
        let store_dms = self.store_dms;
        let respond_to_dms = self.respond_to_dms;

        loop {
            tokio::select! {
                _ = hb_rx.recv() => {
                    let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                    let hb = json!({"op": 1, "d": d});
                    if write.send(Message::Text(hb.to_string().into())).await.is_err() {
                        break;
                    }
                }
                msg = read.next() => {
                    let msg = match msg {
                        Some(Ok(Message::Text(t))) => t,
                        Some(Ok(Message::Ping(payload))) => {
                            if write.send(Message::Pong(payload)).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Err(e)) => {
                            tracing::warn!("DiscordHistory: websocket error: {e}");
                            break;
                        }
                        _ => continue,
                    };

                    let event: serde_json::Value = match serde_json::from_str(msg.as_ref()) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    if let Some(s) = event.get("s").and_then(serde_json::Value::as_i64) {
                        sequence = s;
                    }

                    let op = event.get("op").and_then(serde_json::Value::as_u64).unwrap_or(0);
                    match op {
                        1 => {
                            let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                            let hb = json!({"op": 1, "d": d});
                            if write.send(Message::Text(hb.to_string().into())).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        7 => { tracing::warn!("DiscordHistory: Reconnect (op 7)"); break; }
                        9 => { tracing::warn!("DiscordHistory: Invalid Session (op 9)"); break; }
                        _ => {}
                    }

                    let event_type = event.get("t").and_then(|t| t.as_str()).unwrap_or("");
                    if event_type != "MESSAGE_CREATE" {
                        continue;
                    }

                    let Some(d) = event.get("d") else { continue };

                    // Skip messages from the bot itself
                    let author_id = d
                        .get("author")
                        .and_then(|a| a.get("id"))
                        .and_then(|i| i.as_str())
                        .unwrap_or("");
                    let username = d
                        .get("author")
                        .and_then(|a| a.get("username"))
                        .and_then(|i| i.as_str())
                        .unwrap_or(author_id);

                    if author_id == bot_user_id {
                        continue;
                    }

                    // Skip other bots
                    if d.get("author")
                        .and_then(|a| a.get("bot"))
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    {
                        continue;
                    }

                    let channel_id = d
                        .get("channel_id")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();

                    // DM detection: DMs have no guild_id
                    let is_dm_event = d.get("guild_id").and_then(serde_json::Value::as_str).is_none();

                    // Resolve channel name (with cache)
                    let channel_display = if is_dm_event {
                        "dm".to_string()
                    } else {
                        self.resolve_channel_name(&channel_id).await
                    };

                    if is_dm_event && !store_dms && !respond_to_dms {
                        continue;
                    }

                    // Guild filter
                    if let Some(ref gid) = guild_filter {
                        let msg_guild = d.get("guild_id").and_then(serde_json::Value::as_str);
                        if let Some(g) = msg_guild {
                            if g != gid {
                                continue;
                            }
                        }
                    }

                    // Channel filter
                    if !self.is_channel_watched(&channel_id) {
                        continue;
                    }

                    if !self.is_user_allowed(author_id) {
                        continue;
                    }

                    let content = d.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    let message_id = d.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    let is_mention = contains_bot_mention(content, &bot_user_id);

                    // Collect attachment URLs
                    let attachments: Vec<String> = d
                        .get("attachments")
                        .and_then(|a| a.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|a| a.get("url").and_then(|u| u.as_str()))
                                .map(|u| u.to_string())
                                .collect()
                        })
                        .unwrap_or_default();

                    // Store messages to discord.db (skip DMs if store_dms=false)
                    if (!is_dm_event || store_dms) && (!content.is_empty() || !attachments.is_empty()) {
                        let ts = chrono::Utc::now().to_rfc3339();
                        let mut mem_content = format!(
                            "@{username} in #{channel_display} at {ts}: {content}"
                        );
                        if !attachments.is_empty() {
                            mem_content.push_str(" [attachments: ");
                            mem_content.push_str(&attachments.join(", "));
                            mem_content.push(']');
                        }
                        let mem_key = format!(
                            "discord_{}",
                            if message_id.is_empty() {
                                Uuid::new_v4().to_string()
                            } else {
                                message_id.to_string()
                            }
                        );
                        let channel_id_for_session = if channel_id.is_empty() {
                            None
                        } else {
                            Some(channel_id.as_str())
                        };
                        if let Err(err) = discord_memory
                            .store(
                                &mem_key,
                                &mem_content,
                                MemoryCategory::Custom("discord".to_string()),
                                channel_id_for_session,
                            )
                            .await
                        {
                            tracing::warn!("discord_history: failed to store message: {err}");
                        } else {
                            tracing::debug!(
                                "discord_history: stored message from @{username} in #{channel_display}"
                            );
                        }
                    }

                    // Forward @mention to agent (skip DMs if respond_to_dms=false)
                    if is_mention && (!is_dm_event || respond_to_dms) {
                        let clean_content = strip_bot_mention(content, &bot_user_id);
                        if clean_content.is_empty() {
                            continue;
                        }
                        let channel_msg = ChannelMessage {
                            id: if message_id.is_empty() {
                                Uuid::new_v4().to_string()
                            } else {
                                format!("discord_{message_id}")
                            },
                            sender: author_id.to_string(),
                            reply_target: if channel_id.is_empty() {
                                author_id.to_string()
                            } else {
                                channel_id.clone()
                            },
                            content: clean_content,
                            channel: "discord_history".to_string(),
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                            thread_ts: None,
                            interruption_scope_id: None,
                            attachments: Vec::new(),
                            observe_group: false,
                        };
                        if tx.send(channel_msg).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> bool {
        self.http_client()
            .get("https://discord.com/api/v10/users/@me")
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        let mut guard = self.typing_handles.lock();
        if let Some(h) = guard.remove(recipient) {
            h.abort();
        }
        let client = self.http_client();
        let token = self.bot_token.clone();
        let channel_id = recipient.to_string();
        let handle = tokio::spawn(async move {
            let url = format!("https://discord.com/api/v10/channels/{channel_id}/typing");
            loop {
                let _ = client
                    .post(&url)
                    .header("Authorization", format!("Bot {token}"))
                    .send()
                    .await;
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            }
        });
        guard.insert(recipient.to_string(), handle);
        Ok(())
    }

    async fn stop_typing(&self, recipient: &str) -> anyhow::Result<()> {
        let mut guard = self.typing_handles.lock();
        if let Some(handle) = guard.remove(recipient) {
            handle.abort();
        }
        Ok(())
    }
}
