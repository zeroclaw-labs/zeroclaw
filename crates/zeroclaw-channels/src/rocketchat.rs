use anyhow::{Result, bail};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use urlencoding::encode as urlencode;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_api::media::{MediaAttachment, MediaKind};

/// Rocket.Chat bot channel — polls channel/group history via REST API v1.
pub struct RocketChatChannel {
    server_url: String,
    user_id: String,
    auth_token: String,
    allowed_rooms: Vec<String>,
    allowed_users: Vec<String>,
    dm_replies: bool,
    mention_only: bool,
    thread_replies: bool,
    discussion_replies: bool,
    typing_indicator: bool,
    thinking_placeholder: bool,
    poll_interval_ms: u64,
    download_media: bool,
    max_media_bytes: u64,
    proxy_url: Option<String>,
    typing_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// room_id → human-readable label (populated at startup)
    room_name_cache: Mutex<HashMap<String, String>>,
    /// recipient → placeholder message ID (for thinking_placeholder mode)
    placeholder_msg: Mutex<HashMap<String, String>>,
    /// thread_key ("room_id:tmid") → images received without @mention, held for context
    /// injection when a @mention arrives in the same thread.
    pending_thread_images: Mutex<HashMap<String, Vec<MediaAttachment>>>,
    /// Set via ZC_HIDE_POLL_LOG=1 to suppress per-poll DEBUG lines.
    quiet_poll: bool,
}

impl RocketChatChannel {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        server_url: String,
        user_id: String,
        auth_token: String,
        allowed_rooms: Vec<String>,
        allowed_users: Vec<String>,
        dm_replies: bool,
        mention_only: bool,
        thread_replies: bool,
        discussion_replies: bool,
        typing_indicator: bool,
        poll_interval_ms: u64,
        download_media: bool,
        max_media_bytes: u64,
    ) -> Self {
        let server_url = server_url
            .trim()
            .trim_end_matches('/')
            .trim_end_matches("/api/v1")
            .trim_end_matches("/api")
            .to_string();
        Self {
            server_url,
            user_id,
            auth_token,
            allowed_rooms,
            allowed_users,
            dm_replies,
            mention_only,
            thread_replies,
            discussion_replies,
            typing_indicator,
            thinking_placeholder: false,
            poll_interval_ms,
            download_media,
            max_media_bytes,
            proxy_url: None,
            typing_handle: Mutex::new(None),
            room_name_cache: Mutex::new(HashMap::new()),
            placeholder_msg: Mutex::new(HashMap::new()),
            pending_thread_images: Mutex::new(HashMap::new()),
            quiet_poll: std::env::var("ZC_HIDE_POLL_LOG").is_ok(),
        }
    }

    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    pub fn with_thinking_placeholder(mut self, enabled: bool) -> Self {
        self.thinking_placeholder = enabled;
        self
    }

    fn room_label(&self, room_id: &str) -> String {
        self.room_name_cache
            .lock()
            .get(room_id)
            .map(|n| format!("{n} ({room_id})"))
            .unwrap_or_else(|| room_id.to_string())
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client(
            "channel.rocketchat",
            self.proxy_url.as_deref(),
        )
    }

    fn auth_get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http_client()
            .get(format!("{}/api/v1/{}", self.server_url, path))
            .header("X-Auth-Token", &self.auth_token)
            .header("X-User-Id", &self.user_id)
    }

    fn auth_post(&self, path: &str) -> reqwest::RequestBuilder {
        self.http_client()
            .post(format!("{}/api/v1/{}", self.server_url, path))
            .header("X-Auth-Token", &self.auth_token)
            .header("X-User-Id", &self.user_id)
    }

    async fn get_bot_identity(&self) -> (String, String) {
        let resp: Option<serde_json::Value> =
            async { self.auth_get("me").send().await.ok()?.json().await.ok() }.await;

        let id = resp
            .as_ref()
            .and_then(|v| v.get("_id"))
            .and_then(|i| i.as_str())
            .unwrap_or(&self.user_id)
            .to_string();
        let username = resp
            .as_ref()
            .and_then(|v| v.get("username"))
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string();
        (id, username)
    }

    /// Resolve room name/ID entries in `allowed_rooms` to confirmed room IDs.
    /// Returns `(room_id, endpoint_prefix)` pairs where endpoint_prefix is
    /// `"channels"` or `"groups"` for the correct history endpoint.
    async fn resolve_room_ids(&self) -> Vec<(String, &'static str)> {
        let mut resolved = Vec::new();
        for entry in &self.allowed_rooms {
            if let Some((id, endpoint)) = self.probe_room(entry).await {
                resolved.push((id, endpoint));
            } else {
                tracing::warn!("RocketChat: could not resolve room '{entry}', skipping");
            }
        }
        let dm_rooms = self.discover_dm_rooms().await;
        if !dm_rooms.is_empty() {
            tracing::info!(count = dm_rooms.len(), "RocketChat: discovered DM rooms");
        }
        resolved.extend(dm_rooms);
        resolved
    }

    fn endpoint_for_room_type(room_type: &str) -> &'static str {
        match room_type {
            "p" => "groups",
            "d" => "im",
            _ => "channels",
        }
    }

    fn parse_discussion_rooms_payload(
        &self,
        parent_room_id: &str,
        payload: &serde_json::Value,
    ) -> Vec<(String, &'static str)> {
        payload
            .get("discussions")
            .and_then(|items| items.as_array())
            .map(|items| {
                items.iter()
                    .filter_map(|discussion| {
                        let id = discussion.get("_id").and_then(|v| v.as_str())?;
                        let endpoint = Self::endpoint_for_room_type(
                            discussion.get("t").and_then(|v| v.as_str()).unwrap_or("c"),
                        );
                        let label = discussion
                            .get("fname")
                            .or_else(|| discussion.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or(id)
                            .to_string();
                        self.room_name_cache.lock().insert(id.to_string(), label);
                        Some((id.to_string(), endpoint))
                    })
                    .collect()
            })
            .unwrap_or_else(|| {
                tracing::debug!(
                    "RocketChat: no discussions listed for parent room {}",
                    self.room_label(parent_room_id)
                );
                Vec::new()
            })
    }

    async fn fetch_discussion_rooms_for_parent(
        &self,
        parent_room_id: &str,
    ) -> Vec<(String, &'static str)> {
        let primary_path = format!(
            "rooms.getDiscussions?roomId={}&count=200&offset=0",
            urlencode(parent_room_id)
        );
        if let Ok(resp) = self.auth_get(&primary_path).send().await
            && resp.status().is_success()
            && let Ok(json) = resp.json::<serde_json::Value>().await
        {
            let rooms = self.parse_discussion_rooms_payload(parent_room_id, &json);
            if !rooms.is_empty() {
                return rooms;
            }
        }

        // Rocket.Chat 3.x also exposes `chat.getDiscussions`, which returns
        // thread/discussion markers containing `drid`. Resolve those room IDs
        // back through the normal room probes so we can keep polling them.
        let fallback_path = format!(
            "chat.getDiscussions?roomId={}&count=200&offset=0",
            urlencode(parent_room_id)
        );
        let Ok(resp) = self.auth_get(&fallback_path).send().await else {
            return Vec::new();
        };
        if !resp.status().is_success() {
            return Vec::new();
        }
        let Ok(json) = resp.json::<serde_json::Value>().await else {
            return Vec::new();
        };

        let Some(items) = json.get("messages").and_then(|items| items.as_array()) else {
            return Vec::new();
        };
        let mut resolved = Vec::new();
        let mut seen = HashSet::new();
        for item in items {
            let Some(discussion_room_id) = item.get("drid").and_then(|v| v.as_str()) else {
                continue;
            };
            if !seen.insert(discussion_room_id.to_string()) {
                continue;
            }
            if let Some(room) = self.probe_room(discussion_room_id).await {
                resolved.push(room);
            }
        }
        resolved
    }

    async fn fetch_discussion_rooms(
        &self,
        parent_rooms: &[(String, &'static str)],
    ) -> Vec<(String, &'static str)> {
        if !self.discussion_replies {
            return Vec::new();
        }

        let mut discovered = Vec::new();
        let mut seen = HashSet::new();
        for (room_id, endpoint) in parent_rooms {
            if *endpoint == "im" {
                continue;
            }
            for room in self.fetch_discussion_rooms_for_parent(room_id).await {
                if seen.insert(room.0.clone()) {
                    discovered.push(room);
                }
            }
        }
        discovered
    }

    /// Probe a single room entry by trying channels.info, groups.info, then im.info.
    ///
    /// Supported formats in `allowed_rooms`:
    /// - `"general"` / `"ROOM_ID_123"` — public channel or private group by name or ID
    /// - `"dm:alice"` — direct message with user whose username is "alice"
    async fn probe_room(&self, entry: &str) -> Option<(String, &'static str)> {
        // "dm:<username>" → direct message channel
        if let Some(username) = entry.strip_prefix("dm:") {
            return self.probe_dm_by_username(username).await;
        }

        // Try by roomId first (works for both public and private)
        for endpoint in ["channels", "groups"] {
            let Ok(http_resp) = self
                .auth_get(&format!("{endpoint}.info?roomId={}", urlencode(entry)))
                .send()
                .await
            else {
                continue;
            };
            let Ok(resp): Result<serde_json::Value, _> = http_resp.json().await else {
                continue;
            };
            if resp.get("success").and_then(|s| s.as_bool()).unwrap_or(false) {
                let key = if endpoint == "channels" { "channel" } else { "group" };
                let obj = resp.get(key);
                let id = obj.and_then(|c| c.get("_id")).and_then(|i| i.as_str()).unwrap_or(entry).to_string();
                let name = obj.and_then(|c| c.get("name")).and_then(|n| n.as_str()).unwrap_or(entry).to_string();
                self.room_name_cache.lock().insert(id.clone(), name);
                return Some((id, if endpoint == "channels" { "channels" } else { "groups" }));
            }
        }

        // Try by roomName
        for endpoint in ["channels", "groups"] {
            let Ok(http_resp) = self
                .auth_get(&format!("{endpoint}.info?roomName={}", urlencode(entry)))
                .send()
                .await
            else {
                continue;
            };
            let Ok(resp): Result<serde_json::Value, _> = http_resp.json().await else {
                continue;
            };
            if resp.get("success").and_then(|s| s.as_bool()).unwrap_or(false) {
                let key = if endpoint == "channels" { "channel" } else { "group" };
                let obj = resp.get(key);
                let id = obj.and_then(|c| c.get("_id")).and_then(|i| i.as_str()).unwrap_or(entry).to_string();
                let name = obj.and_then(|c| c.get("name")).and_then(|n| n.as_str()).unwrap_or(entry).to_string();
                self.room_name_cache.lock().insert(id.clone(), name);
                return Some((id, if endpoint == "channels" { "channels" } else { "groups" }));
            }
        }

        // Last resort: try as a DM room ID directly
        let Ok(http_resp) = self
            .auth_get(&format!("im.info?roomId={}", urlencode(entry)))
            .send()
            .await
        else {
            tracing::debug!("RocketChat: probe exhausted all endpoints for '{entry}'");
            return None;
        };
        if let Ok(resp) = http_resp.json::<serde_json::Value>().await
            && resp
                .get("success")
                .and_then(|s| s.as_bool())
                .unwrap_or(false)
        {
            let id = resp
                .get("room")
                .and_then(|r| r.get("_id"))
                .and_then(|i| i.as_str())
                .unwrap_or(entry)
                .to_string();
            return Some((id, "im"));
        }

        tracing::debug!("RocketChat: probe exhausted all endpoints for '{entry}'");
        None
    }

    /// Resolve a DM room by the counterpart's username.
    /// Searches `im.list` (paginated) for an entry whose `usernames` array
    /// contains the target username.  Compatible with all RC versions including
    /// RC 3.x where `im.info?userId=` does not exist.
    async fn probe_dm_by_username(&self, username: &str) -> Option<(String, &'static str)> {
        for offset in [0u32, 200, 400] {
            let Ok(http_resp) = self
                .auth_get(&format!("im.list?count=200&offset={offset}"))
                .send()
                .await
            else {
                break;
            };
            let Ok(resp): Result<serde_json::Value, _> = http_resp.json().await else {
                break;
            };
            let ims = match resp.get("ims").and_then(|i| i.as_array()) {
                Some(a) if !a.is_empty() => a.clone(),
                _ => break,
            };
            for im in &ims {
                let has_user = im
                    .get("usernames")
                    .and_then(|u| u.as_array())
                    .is_some_and(|names| names.iter().any(|n| n.as_str() == Some(username)));
                if has_user {
                    if let Some(id) = im.get("_id").and_then(|i| i.as_str()) {
                        tracing::debug!("RocketChat: found DM with '{username}' → room {id}");
                        self.room_name_cache.lock().insert(id.to_string(), format!("DM:{username}"));
                        return Some((id.to_string(), "im"));
                    }
                }
            }
        }
        tracing::warn!(
            "RocketChat: no DM room found for '{username}' — \
             send a message to the bot in Rocket.Chat first to create the DM room"
        );
        None
    }

    /// Discover DM rooms based on `allowed_users` when `dm_replies = true`.
    /// - Specific usernames → resolve each via `users.info` + `im.info`
    /// - Wildcard `"*"` → fetch all subscribed DMs via `im.list`
    async fn discover_dm_rooms(&self) -> Vec<(String, &'static str)> {
        if !self.dm_replies {
            return vec![];
        }
        let has_wildcard = self.allowed_users.iter().any(|u| u == "*");
        if has_wildcard {
            self.fetch_all_dm_rooms().await
        } else {
            let mut rooms = Vec::new();
            for username in &self.allowed_users {
                if let Some(room) = self.probe_dm_by_username(username).await {
                    rooms.push(room);
                }
            }
            rooms
        }
    }

    /// Fetch all DM rooms the bot is subscribed to (`im.list`).
    async fn fetch_all_dm_rooms(&self) -> Vec<(String, &'static str)> {
        let Ok(http_resp) = self.auth_get("im.list").send().await else {
            tracing::warn!("RocketChat: failed to fetch im.list");
            return vec![];
        };
        let Ok(resp): Result<serde_json::Value, _> = http_resp.json().await else {
            return vec![];
        };
        resp.get("ims")
            .and_then(|ims| ims.as_array())
            .map(|ims| {
                ims.iter()
                    .filter_map(|im| {
                        let id = im.get("_id").and_then(|i| i.as_str())?.to_string();
                        // label as "DM:<other_user>" using the usernames array if available
                        let label = im
                            .get("usernames")
                            .and_then(|u| u.as_array())
                            .and_then(|names| {
                                names.iter().find_map(|n| {
                                    let s = n.as_str()?;
                                    if s != self.user_id { Some(format!("DM:{s}")) } else { None }
                                })
                            })
                            .or_else(|| im.get("name").and_then(|n| n.as_str()).map(|s| format!("DM:{s}")))
                            .unwrap_or_else(|| format!("DM:{id}"));
                        self.room_name_cache.lock().insert(id.clone(), label);
                        Some((id, "im"))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn is_user_allowed(&self, username: &str, user_id: &str) -> bool {
        self.allowed_users
            .iter()
            .any(|u| u == "*" || u == username || u == user_id)
    }

    /// Download a media attachment, enforcing the `max_media_bytes` limit.
    async fn download_attachment(
        &self,
        url: &str,
        file_name: &str,
        mime_type: Option<String>,
    ) -> Option<MediaAttachment> {
        let full_url = if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else {
            // Relative URL — prepend server base
            format!("{}{}", self.server_url, url)
        };

        let resp = match self
            .http_client()
            .get(&full_url)
            .header("X-Auth-Token", &self.auth_token)
            .header("X-User-Id", &self.user_id)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("RocketChat: attachment download failed for '{file_name}': {e}");
                return None;
            }
        };

        if !resp.status().is_success() {
            tracing::warn!(
                "RocketChat: attachment download returned {}: {file_name}",
                resp.status()
            );
            return None;
        }

        if let Some(len) = resp.content_length()
            && len > self.max_media_bytes
        {
            tracing::debug!(
                len,
                max = self.max_media_bytes,
                "RocketChat: attachment too large, skipping: {file_name}"
            );
            return None;
        }

        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("RocketChat: failed to read attachment bytes '{file_name}': {e}");
                return None;
            }
        };

        if bytes.len() as u64 > self.max_media_bytes {
            tracing::debug!(
                "RocketChat: attachment exceeds limit after download, skipping: {file_name}"
            );
            return None;
        }

        Some(MediaAttachment {
            file_name: file_name.to_string(),
            data: bytes.to_vec(),
            mime_type,
        })
    }

    /// Extract a `(url, name, mime)` triple from a single RC attachment object.
    fn attachment_entry(att: &serde_json::Value) -> Option<(String, String, Option<String>)> {
        if let Some(url) = att.get("imageUrl").and_then(|u| u.as_str()) {
            let name = att.get("title").and_then(|t| t.as_str()).unwrap_or("image").to_string();
            let mime = att.get("image_type").and_then(|t| t.as_str()).map(str::to_string);
            Some((url.to_string(), name, mime))
        } else if let Some(url) = att.get("videoUrl").and_then(|u| u.as_str()) {
            let name = att.get("title").and_then(|t| t.as_str()).unwrap_or("video").to_string();
            let mime = att.get("video_type").and_then(|t| t.as_str()).map(str::to_string);
            Some((url.to_string(), name, mime))
        } else if let Some(url) = att.get("audioUrl").and_then(|u| u.as_str()) {
            let name = att.get("title").and_then(|t| t.as_str()).unwrap_or("audio").to_string();
            let mime = att.get("audio_type").and_then(|t| t.as_str()).map(str::to_string);
            Some((url.to_string(), name, mime))
        } else if let Some(url) = att.get("title_link").and_then(|u| u.as_str()) {
            let name = att.get("title").and_then(|t| t.as_str()).unwrap_or("file").to_string();
            Some((url.to_string(), name, None))
        } else {
            None
        }
    }

    /// Collect and download media attachments from a Rocket.Chat message JSON.
    /// Also recurses one level into nested attachments that RC adds for quoted messages.
    async fn collect_attachments(&self, msg: &serde_json::Value) -> Vec<MediaAttachment> {
        if !self.download_media {
            return vec![];
        }

        let Some(atts) = msg.get("attachments").and_then(|a| a.as_array()) else {
            return vec![];
        };

        let mut result = Vec::new();
        for att in atts {
            // Direct media attachment.
            if let Some((url, name, mime)) = Self::attachment_entry(att)
                && let Some(media) = self.download_attachment(&url, &name, mime).await
            {
                result.push(media);
            }

            // Nested attachments: RC wraps quoted-message media one level deeper.
            // e.g. quoting an image puts it in attachments[i].attachments[j].imageUrl
            if let Some(nested) = att.get("attachments").and_then(|a| a.as_array()) {
                for nested_att in nested {
                    if let Some((url, name, mime)) = Self::attachment_entry(nested_att)
                        && let Some(media) = self.download_attachment(&url, &name, mime).await
                    {
                        result.push(media);
                    }
                }
            }
        }
        result
    }

    fn parse_rc_message(
        &self,
        msg: &serde_json::Value,
        bot_user_id: &str,
        bot_username: &str,
        oldest_ts_ms: i64,
        room_id: &str,
        is_dm: bool,
        attachments: Vec<MediaAttachment>,
    ) -> Option<ChannelMessage> {
        let id = msg.get("_id").and_then(|i| i.as_str()).unwrap_or("");

        let sender_id = msg
            .get("u")
            .and_then(|u| u.get("_id"))
            .and_then(|i| i.as_str())
            .unwrap_or("");
        let sender_username = msg
            .get("u")
            .and_then(|u| u.get("username"))
            .and_then(|n| n.as_str())
            .unwrap_or("");

        let raw_text = msg.get("msg").and_then(|m| m.as_str()).unwrap_or("");
        // Strip RC quote-reference links: "[ ](URL)" silent markdown links that RC prepends
        // when a user quotes another message. Keep only the actual reply text.
        let text_owned;
        let text: &str = if raw_text.contains("[ ](") {
            text_owned = strip_rc_quote_links(raw_text);
            &text_owned
        } else {
            raw_text
        };

        let ts_ms = msg.get("ts").map(parse_rc_ts).unwrap_or(0);

        // Skip own messages and messages older than our tracking window
        if sender_id == bot_user_id {
            tracing::debug!("RocketChat: skip msg {id} — own message (bot)");
            return None;
        }
        if ts_ms <= oldest_ts_ms {
            tracing::debug!(
                "RocketChat: skip msg {id} — too old (ts={ts_ms} oldest={oldest_ts_ms})"
            );
            return None;
        }

        // Skip system messages (type field is present for those)
        if let Some(t) = msg.get("t").and_then(|t| t.as_str()) {
            tracing::debug!("RocketChat: skip msg {id} — system message type={t:?}");
            return None;
        }

        // Discussion metadata can show up as `prid` (parent room) or `drid`
        // (discussion room) depending on which side of the discussion message
        // Rocket.Chat returns. Keep this gate local to parsing so the channel
        // can still poll mixed room lists safely.
        let is_discussion_message = msg.get("prid").is_some() || msg.get("drid").is_some();
        if is_discussion_message && !self.discussion_replies {
            tracing::debug!("RocketChat: skip msg {id} — discussion message (discussion_replies=false)");
            return None;
        }

        // Build a descriptive label for media-only messages so the no-reply classifier
        // understands what was sent (e.g. "image" vs "file") and can apply DM-prefer-REPLY.
        let media_label: String;
        let effective_text: &str = if text.trim().is_empty() {
            if attachments.is_empty() {
                tracing::debug!("RocketChat: skip msg {id} — empty text, no attachments");
                return None;
            }
            let mut kinds: Vec<&str> = attachments
                .iter()
                .map(|a| match a.kind() {
                    MediaKind::Image => "image",
                    MediaKind::Audio => "audio file",
                    MediaKind::Video => "video",
                    MediaKind::Unknown => "file",
                })
                .collect();
            kinds.dedup();
            media_label = format!("[User sent {}]", kinds.join(" and "));
            &media_label
        } else {
            text
        };

        if !self.is_user_allowed(sender_username, sender_id) {
            tracing::warn!(
                "RocketChat: skip msg {id} — unauthorized user: username={sender_username:?} \
                 id={sender_id:?} (allowed_users={:?})",
                self.allowed_users
            );
            return None;
        }

        // tmid = parent thread message ID (set when message is a thread reply).
        // Extracted early so the mention_only buffer logic can use it.
        let tmid = msg
            .get("tmid")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        let mentioned_in_metadata = msg
            .get("mentions")
            .and_then(|m| m.as_array())
            .is_some_and(|mentions| {
                mentions.iter().any(|m| {
                    m.get("_id").and_then(|i| i.as_str()) == Some(bot_user_id)
                        || m.get("username").and_then(|u| u.as_str()) == Some(bot_username)
                })
            });

        // mention_only filter:
        //   - DMs: always bypass — only 2 people in the conversation.
        //   - Threads: strict, but buffer images for later @mention context injection.
        //   - Main room: strict.
        let content = if self.mention_only && !is_dm {
            match normalize_rc_content(effective_text, bot_username, mentioned_in_metadata) {
                Some(c) => c,
                None => {
                    // Buffer image attachments from non-mention thread messages so the
                    // next @mention in this thread can reference them.
                    if !tmid.is_empty() && !attachments.is_empty() {
                        let thread_key = format!("{room_id}:{tmid}");
                        let mut buf = self.pending_thread_images.lock();
                        let entry = buf.entry(thread_key).or_default();
                        for att in &attachments {
                            if att.kind() == MediaKind::Image {
                                entry.push(att.clone());
                            }
                        }
                    }
                    tracing::debug!(
                        "RocketChat: skip msg {id} — mention_only=true, bot not mentioned \
                         (bot_username={bot_username:?}, text={effective_text:?})"
                    );
                    return None;
                }
            }
        } else {
            // DM (or mention_only=false): strip @-mention prefix if present but don't filter.
            normalize_rc_content(effective_text, bot_username, mentioned_in_metadata)
                .unwrap_or_else(|| effective_text.to_string())
        };

        // reply_target encodes: "room_id:thread_anchor_id" for threaded replies,
        // "room_id" for top-level.
        let reply_target = if !tmid.is_empty() {
            // Already in a thread — stay in it
            format!("{room_id}:{tmid}")
        } else if self.thread_replies {
            // Start a new thread anchored on this message
            format!("{room_id}:{id}")
        } else {
            room_id.to_string()
        };

        let interruption_scope_id = if !tmid.is_empty() {
            Some(format!("{room_id}:{tmid}"))
        } else {
            None
        };

        // Embed image attachments as [IMAGE:data:...] markers so the vision model
        // receives the actual bytes rather than only a text description.
        // Also inject any images that were buffered from earlier non-mention messages
        // in this thread so the LLM has full visual context when answering a @mention.
        let content = {
            use base64::engine::{Engine, general_purpose::STANDARD};
            let mut buf = content;

            // Inject buffered images from earlier (non-mention) thread messages.
            if !tmid.is_empty() {
                let thread_key = format!("{room_id}:{tmid}");
                let pending = self.pending_thread_images.lock().remove(&thread_key);
                if let Some(prior_images) = pending {
                    for att in &prior_images {
                        let mime = att.mime_type.as_deref().unwrap_or("image/jpeg");
                        let encoded = STANDARD.encode(&att.data);
                        buf.push_str(&format!("\n[IMAGE:data:{mime};base64,{encoded}]"));
                    }
                }
            }

            // Embed images from the current message.
            for att in &attachments {
                if att.kind() == MediaKind::Image {
                    let mime = att.mime_type.as_deref().unwrap_or("image/jpeg");
                    let encoded = STANDARD.encode(&att.data);
                    buf.push_str(&format!("\n[IMAGE:data:{mime};base64,{encoded}]"));
                }
            }
            buf
        };

        Some(ChannelMessage {
            id: format!("rocketchat_{id}"),
            sender: sender_username.to_string(),
            reply_target,
            content,
            channel: "rocketchat".to_string(),
            #[allow(clippy::cast_sign_loss)]
            timestamp: (ts_ms.max(0) / 1000) as u64,
            thread_ts: if tmid.is_empty() { None } else { Some(tmid) },
            interruption_scope_id,
            attachments,
        })
    }

    /// Fetch history for one room, trying channels.history then groups.history.
    async fn fetch_history(
        &self,
        room_id: &str,
        endpoint: &str,
        oldest: &str,
    ) -> Option<serde_json::Value> {
        if !self.quiet_poll {
            tracing::debug!("RocketChat: polling {endpoint}.history {} oldest={oldest}", self.room_label(room_id));
        }
        let resp = self
            .auth_get(&format!(
                "{endpoint}.history?roomId={room_id}&oldest={oldest}&count=200&inclusive=false"
            ))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                let json: serde_json::Value = r.json().await.ok()?;
                if !self.quiet_poll {
                    let count = json
                        .get("messages")
                        .and_then(|m| m.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    tracing::debug!(
                        "RocketChat: {endpoint}.history {} → {count} from API",
                        self.room_label(room_id)
                    );
                }
                Some(json)
            }
            Ok(r) => {
                tracing::warn!(
                    "RocketChat: {endpoint}.history returned {} for {room_id}",
                    r.status()
                );
                None
            }
            Err(e) => {
                tracing::warn!("RocketChat: {endpoint}.history error for {room_id}: {e}");
                None
            }
        }
    }

    /// POST chat.react — add or remove a reaction on a message.
    /// `message_id` may carry the "rocketchat_" prefix added by parse_rc_message;
    /// we strip it before calling the API.
    async fn chat_react(&self, message_id: &str, emoji: &str, should_react: bool) -> Result<()> {
        let raw_id = message_id
            .strip_prefix("rocketchat_")
            .unwrap_or(message_id);
        let emoji_name = unicode_emoji_to_rc_name(emoji);
        let body = serde_json::json!({
            "messageId": raw_id,
            "emoji": emoji_name,
            "shouldReact": should_react,
        });
        let resp = self.auth_post("chat.react").json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let sanitized = zeroclaw_providers::sanitize_api_error(&text);
            anyhow::bail!("RocketChat chat.react failed ({status}): {sanitized}");
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for RocketChatChannel {
    fn name(&self) -> &str {
        "rocketchat"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        // recipient encodes "room_id" or "room_id:thread_anchor_id"
        let (room_id, tmid) = if let Some((r, t)) = message.recipient.split_once(':') {
            (r, Some(t))
        } else {
            (message.recipient.as_str(), None)
        };

        // Look up placeholder by the full recipient key (includes ":tmid" for threads/discussions)
        // so the placeholder that was posted inside the thread is edited, not one in the main room.
        let placeholder_id = self.placeholder_msg.lock().remove(message.recipient.as_str());
        if let Some(ref ph_id) = placeholder_id {
            let edit_body = serde_json::json!({
                "roomId": room_id,
                "msgId": ph_id,
                "text": message.content,
            });
            let resp = self
                .auth_post("chat.update")
                .json(&edit_body)
                .send()
                .await?;
            let status = resp.status();
            if status.is_success() {
                return Ok(());
            }
            tracing::debug!(
                "RocketChat: chat.update failed ({status}), falling back to sendMessage"
            );
        }

        let mut msg_obj = serde_json::json!({
            "rid": room_id,
            "msg": message.content,
        });

        if let Some(tid) = tmid {
            msg_obj.as_object_mut().expect("json object").insert(
                "tmid".to_string(),
                serde_json::Value::String(tid.to_string()),
            );
        }

        let resp = self
            .auth_post("chat.sendMessage")
            .json(&serde_json::json!({"message": msg_obj}))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response: {e}>"));
            bail!("RocketChat sendMessage failed ({status}): {body}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        if self.allowed_rooms.is_empty() {
            bail!("RocketChat: allowed_rooms must list at least one room ID or name");
        }

        let (bot_user_id, bot_username) = self.get_bot_identity().await;

        let mut rooms = self.resolve_room_ids().await;
        if rooms.is_empty() {
            bail!("RocketChat: no valid rooms could be resolved from allowed_rooms");
        }
        let parent_rooms: Vec<(String, &'static str)> = rooms
            .iter()
            .filter(|(_, endpoint)| *endpoint != "im")
            .cloned()
            .collect();
        let discussion_rooms = self.fetch_discussion_rooms(&parent_rooms).await;
        let mut known_room_ids: HashSet<String> = rooms.iter().map(|(id, _)| id.clone()).collect();
        for (room_id, endpoint) in discussion_rooms {
            if known_room_ids.insert(room_id.clone()) {
                rooms.push((room_id, endpoint));
            }
        }

        tracing::info!(
            rooms = rooms.len(),
            poll_interval_ms = self.poll_interval_ms,
            "RocketChat channel listening..."
        );
        for (room_id, endpoint) in &rooms {
            tracing::info!("RocketChat: watching [{endpoint}] {}", self.room_label(room_id));
        }

        // Start from now so we don't replay historical messages on startup
        #[allow(clippy::cast_possible_truncation)]
        let start_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let mut last_ts: HashMap<String, i64> =
            rooms.iter().map(|(id, _)| (id.clone(), start_ms)).collect();
        // Track processed message IDs to suppress RC re-delivering edited messages
        // (RC bumps _updatedAt on edits, causing them to reappear in history polls).
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        let poll_interval = std::time::Duration::from_millis(self.poll_interval_ms);

        loop {
            tokio::time::sleep(poll_interval).await;

            if self.discussion_replies {
                for (room_id, endpoint) in self.fetch_discussion_rooms(&parent_rooms).await {
                    if known_room_ids.insert(room_id.clone()) {
                        tracing::info!(
                            "RocketChat: watching [{}] {} (discussion of allowed room)",
                            endpoint,
                            self.room_label(&room_id)
                        );
                        last_ts.insert(room_id.clone(), start_ms);
                        rooms.push((room_id, endpoint));
                    }
                }
            }

            for (room_id, endpoint) in &rooms {
                let oldest_ms = *last_ts.get(room_id).unwrap_or(&start_ms);
                let oldest_str = ms_to_iso8601(oldest_ms);

                let Some(data) = self.fetch_history(room_id, endpoint, &oldest_str).await else {
                    continue;
                };

                let Some(messages) = data.get("messages").and_then(|m| m.as_array()).cloned()
                else {
                    continue;
                };

                // Sort chronologically before processing
                let mut sorted = messages;
                sorted.sort_by_key(|m| m.get("ts").map(parse_rc_ts).unwrap_or(0));

                let oldest_before = oldest_ms;
                let mut forwarded = 0usize;
                for msg in &sorted {
                    let ts = msg.get("ts").map(parse_rc_ts).unwrap_or(0);

                    let entry = last_ts.entry(room_id.clone()).or_insert(start_ms);
                    *entry = (*entry).max(ts);

                    let msg_id = msg.get("_id").and_then(|i| i.as_str()).unwrap_or("?");

                    if !seen_ids.insert(msg_id.to_string()) {
                        if !self.quiet_poll {
                            tracing::debug!("RocketChat: skip msg {msg_id} — already processed (edited re-delivery)");
                        }
                        continue;
                    }
                    let sender = msg
                        .get("u")
                        .and_then(|u| u.get("username"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("?");
                    let text = msg.get("msg").and_then(|m| m.as_str()).unwrap_or("");
                    tracing::debug!(
                        "RocketChat: raw msg id={msg_id} from={sender} room={room_id} ts={ts} \
                         text={text}"
                    );

                    let attachments = self.collect_attachments(msg).await;

                    if let Some(channel_msg) = self.parse_rc_message(
                        msg,
                        &bot_user_id,
                        &bot_username,
                        oldest_before,
                        room_id,
                        *endpoint == "im",
                        attachments,
                    ) {
                        forwarded += 1;
                        tracing::info!(
                            "RocketChat: ✉ forwarding msg id={msg_id} from={} \
                             reply_target={}",
                            channel_msg.sender,
                            channel_msg.reply_target,
                        );
                        if tx.send(channel_msg).await.is_err() {
                            return Ok(());
                        }
                    }
                }
                if forwarded > 0 {
                    tracing::info!(
                        "RocketChat: {endpoint}.history {} → {forwarded} new message(s)",
                        self.room_label(room_id)
                    );
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        self.auth_get("me")
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn start_typing(&self, recipient: &str) -> Result<()> {
        self.stop_typing(recipient).await?;

        let (room_id, tmid_opt) = match recipient.split_once(':') {
            Some((r, t)) => (r.to_string(), Some(t.to_string())),
            None => (recipient.to_string(), None),
        };

        // Rocket.Chat 5.0+ native typing indicator (optional — requires typing_indicator=true).
        // chat.typing operates on the room level; threads/discussions share the parent indicator.
        if self.typing_indicator {
            let client = self.http_client();
            let token = self.auth_token.clone();
            let user_id_header = self.user_id.clone();
            let server_url = self.server_url.clone();
            let room_id_for_typing = room_id.clone();
            let handle = tokio::spawn(async move {
                let url = format!("{server_url}/api/v1/chat.typing");
                loop {
                    let body = serde_json::json!({"roomId": room_id_for_typing, "status": true});
                    if let Ok(r) = client
                        .post(&url)
                        .header("X-Auth-Token", &token)
                        .header("X-User-Id", &user_id_header)
                        .json(&body)
                        .send()
                        .await
                        && !r.status().is_success()
                    {
                        tracing::debug!(
                            status = %r.status(),
                            "RocketChat typing indicator failed (requires RC >= 5.0)"
                        );
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                }
            });
            *self.typing_handle.lock() = Some(handle);
        }

        // Thinking placeholder: keyed by the full recipient ("room_id" or "room_id:tmid") so
        // that thread/discussion replies get their own placeholder placed inside the correct
        // thread or discussion room rather than the parent room.
        if self.thinking_placeholder && !self.placeholder_msg.lock().contains_key(recipient) {
            match self.send_placeholder(&room_id, tmid_opt.as_deref()).await {
                Some(ph_id) => {
                    tracing::info!(
                        "RocketChat: sent ⏳ placeholder recipient={recipient} msg_id={ph_id}"
                    );
                    self.placeholder_msg.lock().insert(recipient.to_string(), ph_id);
                }
                None => {
                    tracing::warn!(
                        "RocketChat: failed to send ⏳ placeholder for recipient={recipient}"
                    );
                }
            }
        }

        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> Result<()> {
        if let Some(handle) = self.typing_handle.lock().take() {
            handle.abort();
        }
        // Do NOT delete the ⏳ placeholder here. The orchestrator calls stop_typing just
        // before send(), so send() will find the placeholder ID in placeholder_msg and edit
        // it with the real reply. For no-reply messages the typing task is never spawned,
        // so placeholder_msg is always empty here.
        Ok(())
    }

    async fn add_reaction(&self, _channel_id: &str, message_id: &str, emoji: &str) -> Result<()> {
        self.chat_react(message_id, emoji, true).await
    }

    async fn remove_reaction(
        &self,
        _channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<()> {
        self.chat_react(message_id, emoji, false).await
    }
}

const THINKING_PLACEHOLDERS: &[&str] = &[
    "คิดแปปน้าา… :cat_bongo:",
    "กำลังประมวลผลแบบตึงๆ… :work_hard:",
    "ไอเดียกำลังมาา… :cat_rolling:",
    "กำลังคิดอยู่… :amg_release:",
    "ไอเดียกำลังเดินทาง… :bugcat_out:",
];

static PLACEHOLDER_COUNTER: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

impl RocketChatChannel {
    async fn send_placeholder(&self, room_id: &str, tmid: Option<&str>) -> Option<String> {
        let idx = PLACEHOLDER_COUNTER
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % THINKING_PLACEHOLDERS.len();
        let text = THINKING_PLACEHOLDERS[idx];
        let mut msg_obj = serde_json::json!({
            "rid": room_id,
            "msg": text,
        });
        if let Some(tid) = tmid {
            msg_obj
                .as_object_mut()
                .expect("json object")
                .insert("tmid".to_string(), serde_json::Value::String(tid.to_string()));
        }
        let resp = self
            .auth_post("chat.sendMessage")
            .json(&serde_json::json!({"message": msg_obj}))
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let json: serde_json::Value = resp.json().await.ok()?;
        json.get("message")
            .and_then(|m| m.get("_id"))
            .and_then(|i| i.as_str())
            .map(str::to_string)
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Remove RC quote-reference links from message text.
///
/// Map a Unicode emoji codepoint to a RocketChat `:emoji_name:` string.
/// RC's chat.react endpoint expects the colon-delimited name format.
/// Unmapped emoji are passed through as-is (RC may accept some natively).
fn unicode_emoji_to_rc_name(emoji: &str) -> String {
    match emoji {
        "👀" => ":eyes:".to_string(),
        "✅" => ":white_check_mark:".to_string(),
        "⚠️" | "⚠" => ":warning:".to_string(),
        "👍" => ":thumbsup:".to_string(),
        "🚫" => ":no_entry_sign:".to_string(),
        other => other.to_string(),
    }
}

/// When a user quotes a message in RC, the server prepends a silent markdown
/// link `[ ](https://…?msg=ID)` to `msg.text`.  This strips all such tokens
/// so the bot only sees the actual reply text.
fn strip_rc_quote_links(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remainder = text;
    loop {
        match remainder.find("[ ](") {
            None => {
                result.push_str(remainder);
                break;
            }
            Some(start) => {
                // Keep text before the link token (if any).
                result.push_str(&remainder[..start]);
                let after_open = &remainder[start + 4..]; // skip "[ ]("
                match after_open.find(')') {
                    None => {
                        // Malformed — keep remainder as-is.
                        result.push_str(&remainder[start..]);
                        break;
                    }
                    Some(end) => {
                        remainder = &after_open[end + 1..]; // skip past ")"
                        // Consume a single trailing space between tokens.
                        if remainder.starts_with(' ') {
                            remainder = &remainder[1..];
                        }
                    }
                }
            }
        }
    }
    result.trim().to_string()
}

/// Parse a Rocket.Chat timestamp field to Unix milliseconds.
///
/// RC REST API uses MongoDB extended JSON, but different server versions and
/// endpoints return timestamps in different shapes:
/// - `{"$date": 1234567890000}`       ← EJSON numeric (most common)
/// - `{"$date": "2026-04-28T..."}`    ← EJSON string (some older builds)
/// - `"2026-04-28T00:46:13.000Z"`     ← plain ISO-8601 string
/// - `1234567890000`                   ← plain integer
fn parse_rc_ts(ts: &serde_json::Value) -> i64 {
    // EJSON numeric
    if let Some(ms) = ts.get("$date").and_then(|d| d.as_i64()) {
        return ms;
    }
    // EJSON string
    if let Some(s) = ts.get("$date").and_then(|d| d.as_str()) {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return dt.timestamp_millis();
        }
    }
    // Plain ISO-8601 string
    if let Some(s) = ts.as_str() {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return dt.timestamp_millis();
        }
    }
    // Plain integer
    if let Some(ms) = ts.as_i64() {
        return ms;
    }
    0
}

/// Convert Unix milliseconds to an ISO 8601 string accepted by RC's `oldest` query param.
fn ms_to_iso8601(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

/// Strip @-mentions of the bot username and trim the result.
/// Returns `None` if the message has no mention and `mentioned_in_metadata` is false.
fn normalize_rc_content(
    text: &str,
    bot_username: &str,
    mentioned_in_metadata: bool,
) -> Option<String> {
    let spans = find_mention_spans(text, bot_username);
    if spans.is_empty() && !mentioned_in_metadata {
        return None;
    }

    if spans.is_empty() {
        // Mentioned via metadata (e.g. a DM) — preserve text as-is
        let trimmed = text.trim().to_string();
        return if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        };
    }

    // Strip every @mention span
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;
    for (start, end) in &spans {
        result.push_str(&text[cursor..*start]);
        result.push(' ');
        cursor = *end;
    }
    result.push_str(&text[cursor..]);
    let cleaned = result.trim().to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn is_rc_username_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
}

/// Find byte-offset spans of `@<bot_username>` in `text` (case-insensitive,
/// only matches at a username-character boundary after the mention).
fn find_mention_spans(text: &str, bot_username: &str) -> Vec<(usize, usize)> {
    if bot_username.is_empty() {
        return vec![];
    }

    let mention = format!("@{}", bot_username.to_ascii_lowercase());
    let ml = mention.len();
    let mention_bytes = mention.as_bytes();
    let text_bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;

    while i + ml <= text_bytes.len() {
        let is_match = text_bytes[i] == b'@'
            && text_bytes[i..i + ml]
                .iter()
                .zip(mention_bytes.iter())
                .all(|(a, b)| a.eq_ignore_ascii_case(b));

        if is_match {
            let end = i + ml;
            let at_boundary = text[end..]
                .chars()
                .next()
                .is_none_or(|c| !is_rc_username_char(c));
            if at_boundary {
                spans.push((i, end));
                i = end;
                continue;
            }
        }
        i += text[i..].chars().next().map_or(1, char::len_utf8);
    }

    spans
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_channel(allowed_users: Vec<String>, thread_replies: bool) -> RocketChatChannel {
        RocketChatChannel::new(
            "https://chat.example.com".into(),
            "bot_user_id".into(),
            "bot_token".into(),
            vec!["room1".into()],
            allowed_users,
            false, // dm_replies
            false, // mention_only
            thread_replies,
            true,
            true,
            1500,
            false,
            25 * 1024 * 1024,
        )
    }

    fn make_mention_only() -> RocketChatChannel {
        RocketChatChannel::new(
            "https://chat.example.com".into(),
            "bot_user_id".into(),
            "bot_token".into(),
            vec!["room1".into()],
            vec!["*".into()],
            false, // dm_replies
            true,  // mention_only
            true,
            true,
            true,
            1500,
            false,
            25 * 1024 * 1024,
        )
    }

    fn make_discussion_channel(discussion_replies: bool) -> RocketChatChannel {
        RocketChatChannel::new(
            "https://chat.example.com".into(),
            "bot_user_id".into(),
            "bot_token".into(),
            vec!["room1".into()],
            vec!["*".into()],
            false,
            false,
            false,
            discussion_replies,
            true,
            1500,
            false,
            25 * 1024 * 1024,
        )
    }

    fn ts(ms: i64) -> serde_json::Value {
        json!({"$date": ms})
    }

    #[test]
    fn parse_discussion_rooms_payload_extracts_children() {
        let ch = make_discussion_channel(true);
        let payload = json!({
            "discussions": [
                {
                    "_id": "discussion_public",
                    "fname": "Design Thread",
                    "prid": "room1",
                    "t": "c"
                },
                {
                    "_id": "discussion_private",
                    "name": "secret-child",
                    "prid": "room1",
                    "t": "p"
                }
            ],
            "success": true
        });

        let rooms = ch.parse_discussion_rooms_payload("room1", &payload);
        assert_eq!(
            rooms,
            vec![
                ("discussion_public".to_string(), "channels"),
                ("discussion_private".to_string(), "groups")
            ]
        );
        assert_eq!(
            ch.room_name_cache.lock().get("discussion_public").cloned(),
            Some("Design Thread".to_string())
        );
        assert_eq!(
            ch.room_name_cache.lock().get("discussion_private").cloned(),
            Some("secret-child".to_string())
        );
    }

    #[test]
    fn parse_discussion_rooms_payload_handles_empty_payload() {
        let ch = make_discussion_channel(true);
        let payload = json!({"success": true});
        assert!(ch.parse_discussion_rooms_payload("room1", &payload).is_empty());
    }

    #[test]
    fn server_url_trailing_slash_stripped() {
        let ch = make_channel(vec!["*".into()], false);
        assert_eq!(ch.server_url, "https://chat.example.com");
    }

    #[test]
    fn server_url_api_suffix_stripped() {
        let ch = RocketChatChannel::new(
            "https://chat.example.com/api/v1".into(),
            "bot_user_id".into(),
            "bot_token".into(),
            vec!["room1".into()],
            vec!["*".into()],
            false,
            false,
            true,
            true,
            true,
            1500,
            false,
            25 * 1024 * 1024,
        );
        assert_eq!(ch.server_url, "https://chat.example.com");
    }

    #[test]
    fn allowlist_wildcard() {
        let ch = make_channel(vec!["*".into()], false);
        assert!(ch.is_user_allowed("alice", "uid1"));
        assert!(ch.is_user_allowed("bob", "uid2"));
    }

    #[test]
    fn allowlist_by_username() {
        let ch = make_channel(vec!["alice".into()], false);
        assert!(ch.is_user_allowed("alice", "uid1"));
        assert!(!ch.is_user_allowed("bob", "uid2"));
    }

    #[test]
    fn allowlist_by_user_id() {
        let ch = make_channel(vec!["uid_abc".into()], false);
        assert!(ch.is_user_allowed("anyone", "uid_abc"));
        assert!(!ch.is_user_allowed("anyone", "uid_xyz"));
    }

    #[test]
    fn allowlist_empty_denies_all() {
        let ch = make_channel(vec![], false);
        assert!(!ch.is_user_allowed("alice", "uid1"));
    }

    // ── parse_rc_message ─────────────────────────────────────────────────────

    #[test]
    fn parse_basic_message() {
        let ch = make_channel(vec!["*".into()], true);
        let msg = json!({
            "_id": "msg1",
            "rid": "room1",
            "msg": "hello world",
            "ts": ts(1_600_000_001_000_i64),
            "u": {"_id": "user1", "username": "alice", "name": "Alice"},
        });
        let result = ch.parse_rc_message(
            &msg,
            "bot_user_id",
            "mybot",
            1_600_000_000_000_i64,
            "room1",
            false,
            vec![],
        );
        let out = result.unwrap();
        assert_eq!(out.sender, "alice"); // username, not user_id
        assert_eq!(out.content, "hello world");
        assert_eq!(out.reply_target, "room1:msg1"); // thread_replies=true
        assert_eq!(out.id, "rocketchat_msg1");
    }

    #[test]
    fn parse_ignores_own_message() {
        let ch = make_channel(vec!["*".into()], false);
        let msg = json!({
            "_id": "msg1",
            "msg": "I said this",
            "ts": ts(1_600_000_001_000_i64),
            "u": {"_id": "bot_user_id", "username": "mybot"},
        });
        assert!(
            ch.parse_rc_message(
                &msg,
                "bot_user_id",
                "mybot",
                1_600_000_000_000_i64,
                "room1",
                false,
                vec![]
            )
            .is_none()
        );
    }

    #[test]
    fn parse_ignores_old_message() {
        let ch = make_channel(vec!["*".into()], false);
        let msg = json!({
            "_id": "old",
            "msg": "stale",
            "ts": ts(1_000_000_000_i64),
            "u": {"_id": "user1", "username": "alice"},
        });
        assert!(
            ch.parse_rc_message(
                &msg,
                "bot_user_id",
                "mybot",
                1_600_000_000_000_i64,
                "room1",
                false,
                vec![]
            )
            .is_none()
        );
    }

    #[test]
    fn parse_ignores_system_message() {
        let ch = make_channel(vec!["*".into()], false);
        let msg = json!({
            "_id": "sys1",
            "msg": "joined",
            "ts": ts(1_600_000_001_000_i64),
            "u": {"_id": "user1", "username": "alice"},
            "t": "uj",  // system: user joined
        });
        assert!(
            ch.parse_rc_message(
                &msg,
                "bot_user_id",
                "mybot",
                1_600_000_000_000_i64,
                "room1",
                false,
                vec![]
            )
            .is_none()
        );
    }

    #[test]
    fn parse_thread_reply_stays_in_thread() {
        let ch = make_channel(vec!["*".into()], false);
        let msg = json!({
            "_id": "reply1",
            "msg": "hi",
            "ts": ts(1_600_000_001_000_i64),
            "u": {"_id": "user1", "username": "alice"},
            "tmid": "thread_root",
        });
        let out = ch
            .parse_rc_message(
                &msg,
                "bot_user_id",
                "mybot",
                1_600_000_000_000_i64,
                "room1",
                false,
                vec![],
            )
            .unwrap();
        assert_eq!(out.reply_target, "room1:thread_root");
        assert_eq!(out.thread_ts.as_deref(), Some("thread_root"));
        assert_eq!(
            out.interruption_scope_id.as_deref(),
            Some("room1:thread_root")
        );
    }

    #[test]
    fn parse_thread_replies_false_stays_at_room_level() {
        let ch = make_channel(vec!["*".into()], false);
        let msg = json!({
            "_id": "msg2",
            "msg": "hi",
            "ts": ts(1_600_000_001_000_i64),
            "u": {"_id": "user1", "username": "alice"},
        });
        let out = ch
            .parse_rc_message(
                &msg,
                "bot_user_id",
                "mybot",
                1_600_000_000_000_i64,
                "room1",
                false,
                vec![],
            )
            .unwrap();
        // thread_replies=false, no tmid → room-level reply
        assert_eq!(out.reply_target, "room1");
    }

    #[test]
    fn discussion_replies_false_skips_discussion_messages() {
        let ch = make_discussion_channel(false);
        let msg = json!({
            "_id": "discussion_msg",
            "msg": "discussion text",
            "ts": ts(1_600_000_001_000_i64),
            "u": {"_id": "user1", "username": "alice"},
            "prid": "parent_room",
        });

        assert!(
            ch.parse_rc_message(
                &msg,
                "bot_user_id",
                "mybot",
                1_600_000_000_000_i64,
                "room1",
                false,
                vec![]
            )
            .is_none()
        );
    }

    #[test]
    fn discussion_replies_true_keeps_discussion_room_scope() {
        let ch = make_discussion_channel(true);
        let msg = json!({
            "_id": "discussion_msg",
            "msg": "discussion text",
            "ts": ts(1_600_000_001_000_i64),
            "u": {"_id": "user1", "username": "alice"},
            "prid": "parent_room",
        });

        let out = ch
            .parse_rc_message(
                &msg,
                "bot_user_id",
                "mybot",
                1_600_000_000_000_i64,
                "room1",
                false,
                vec![],
            )
            .unwrap();
        assert_eq!(out.reply_target, "room1");
    }

    // ── mention_only ──────────────────────────────────────────────────────────

    #[test]
    fn mention_only_skips_without_mention() {
        let ch = make_mention_only();
        let msg = json!({
            "_id": "m1",
            "msg": "hello everyone",
            "ts": ts(1_600_000_001_000_i64),
            "u": {"_id": "user1", "username": "alice"},
        });
        assert!(
            ch.parse_rc_message(
                &msg,
                "bot_user_id",
                "mybot",
                1_600_000_000_000_i64,
                "room1",
                false,
                vec![]
            )
            .is_none()
        );
    }

    #[test]
    fn mention_only_accepts_at_mention() {
        let ch = make_mention_only();
        let msg = json!({
            "_id": "m1",
            "msg": "@mybot what is 2+2?",
            "ts": ts(1_600_000_001_000_i64),
            "u": {"_id": "user1", "username": "alice"},
            "mentions": [{"_id": "bot_user_id", "username": "mybot"}],
        });
        let out = ch
            .parse_rc_message(
                &msg,
                "bot_user_id",
                "mybot",
                1_600_000_000_000_i64,
                "room1",
                false,
                vec![],
            )
            .unwrap();
        assert_eq!(out.content, "what is 2+2?");
    }

    #[test]
    fn mention_only_strips_mention_and_trims() {
        let ch = make_mention_only();
        let msg = json!({
            "_id": "m1",
            "msg": "  @mybot  run status  ",
            "ts": ts(1_600_000_001_000_i64),
            "u": {"_id": "user1", "username": "alice"},
            "mentions": [{"_id": "bot_user_id", "username": "mybot"}],
        });
        let out = ch
            .parse_rc_message(
                &msg,
                "bot_user_id",
                "mybot",
                1_600_000_000_000_i64,
                "room1",
                false,
                vec![],
            )
            .unwrap();
        assert_eq!(out.content, "run status");
    }

    #[test]
    fn mention_only_rejects_empty_after_stripping() {
        let ch = make_mention_only();
        let msg = json!({
            "_id": "m1",
            "msg": "@mybot",
            "ts": ts(1_600_000_001_000_i64),
            "u": {"_id": "user1", "username": "alice"},
            "mentions": [{"_id": "bot_user_id", "username": "mybot"}],
        });
        assert!(
            ch.parse_rc_message(
                &msg,
                "bot_user_id",
                "mybot",
                1_600_000_000_000_i64,
                "room1",
                false,
                vec![]
            )
            .is_none()
        );
    }

    #[test]
    fn mention_only_metadata_only_preserves_text() {
        let ch = make_mention_only();
        let msg = json!({
            "_id": "m1",
            "msg": "check this out",
            "ts": ts(1_600_000_001_000_i64),
            "u": {"_id": "user1", "username": "alice"},
            "mentions": [{"_id": "bot_user_id", "username": "mybot"}],
        });
        let out = ch
            .parse_rc_message(
                &msg,
                "bot_user_id",
                "mybot",
                1_600_000_000_000_i64,
                "room1",
                false,
                vec![],
            )
            .unwrap();
        assert_eq!(out.content, "check this out");
    }

    #[test]
    fn mention_only_case_insensitive() {
        let ch = make_mention_only();
        let msg = json!({
            "_id": "m1",
            "msg": "@MyBot hello",
            "ts": ts(1_600_000_001_000_i64),
            "u": {"_id": "user1", "username": "alice"},
        });
        let out = ch
            .parse_rc_message(
                &msg,
                "bot_user_id",
                "mybot",
                1_600_000_000_000_i64,
                "room1",
                false,
                vec![],
            )
            .unwrap();
        assert_eq!(out.content, "hello");
    }

    #[test]
    fn mention_only_word_boundary_no_partial() {
        let ch = make_mention_only();
        let msg = json!({
            "_id": "m1",
            "msg": "@mybotextended hello",
            "ts": ts(1_600_000_001_000_i64),
            "u": {"_id": "user1", "username": "alice"},
        });
        // "@mybotextended" should NOT match "@mybot"
        assert!(
            ch.parse_rc_message(
                &msg,
                "bot_user_id",
                "mybot",
                1_600_000_000_000_i64,
                "room1",
                false,
                vec![]
            )
            .is_none()
        );
    }

    // ── normalize_rc_content ─────────────────────────────────────────────────

    #[test]
    fn normalize_strips_and_trims() {
        assert_eq!(
            normalize_rc_content("  @mybot  do stuff  ", "mybot", false),
            Some("do stuff".to_string())
        );
    }

    #[test]
    fn normalize_none_for_no_mention() {
        assert!(normalize_rc_content("hello world", "mybot", false).is_none());
    }

    #[test]
    fn normalize_none_when_only_mention() {
        assert!(normalize_rc_content("@mybot", "mybot", false).is_none());
    }

    #[test]
    fn normalize_preserves_text_for_metadata_mention() {
        assert_eq!(
            normalize_rc_content("check this out", "mybot", true),
            Some("check this out".to_string())
        );
    }

    #[test]
    fn normalize_strips_multiple_mentions() {
        let result = normalize_rc_content("@mybot hello @mybot world", "mybot", false);
        assert_eq!(result.as_deref(), Some("hello   world"));
    }

    // ── ms_to_iso8601 ─────────────────────────────────────────────────────────

    #[test]
    fn iso8601_epoch() {
        assert_eq!(ms_to_iso8601(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn iso8601_known_timestamp() {
        // 2023-01-01T00:00:00.000Z = 1672531200000 ms
        assert_eq!(ms_to_iso8601(1_672_531_200_000), "2023-01-01T00:00:00.000Z");
    }

    #[test]
    fn iso8601_preserves_millis() {
        // 1_672_531_200_123 ms → should have .123 milliseconds
        let s = ms_to_iso8601(1_672_531_200_123);
        assert!(s.ends_with(".123Z"), "got: {s}");
    }

    // ── find_mention_spans ───────────────────────────────────────────────────

    #[test]
    fn find_spans_at_start() {
        let spans = find_mention_spans("@mybot hello", "mybot");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0], (0, 6));
    }

    #[test]
    fn find_spans_at_end() {
        let spans = find_mention_spans("hello @mybot", "mybot");
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn find_spans_no_match() {
        assert!(find_mention_spans("hello world", "mybot").is_empty());
    }

    #[test]
    fn find_spans_partial_username_no_match() {
        assert!(find_mention_spans("@mybotextended", "mybot").is_empty());
    }

    #[test]
    fn find_spans_followed_by_punctuation() {
        // comma is not a username char → boundary
        let spans = find_mention_spans("@mybot, hello", "mybot");
        assert_eq!(spans.len(), 1);
    }
}
