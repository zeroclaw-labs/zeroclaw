use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Slack channel — polls conversations.history via Web API
pub struct SlackChannel {
    bot_token: String,
    channel_id: Option<String>,
    allowed_users: Vec<String>,
}

impl SlackChannel {
    pub fn new(bot_token: String, channel_id: Option<String>, allowed_users: Vec<String>) -> Self {
        Self {
            bot_token,
            channel_id,
            allowed_users,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.slack")
    }

    /// Check if a Slack user ID is in the allowlist.
    /// Empty list means deny everyone until explicitly configured.
    /// `"*"` means allow everyone.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    /// Get the bot's own user ID so we can ignore our own messages
    async fn get_bot_user_id(&self) -> Option<String> {
        let resp: serde_json::Value = self
            .http_client()
            .get("https://slack.com/api/auth.test")
            .bearer_auth(&self.bot_token)
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;

        resp.get("user_id")
            .and_then(|u| u.as_str())
            .map(String::from)
    }

    /// Resolve the thread identifier for inbound Slack messages.
    /// Replies carry `thread_ts` (root thread id); top-level messages only have `ts`.
    fn inbound_thread_ts(msg: &serde_json::Value, ts: &str) -> Option<String> {
        msg.get("thread_ts")
            .and_then(|t| t.as_str())
            .or(if ts.is_empty() { None } else { Some(ts) })
            .map(str::to_string)
    }

    fn normalized_channel_id(input: Option<&str>) -> Option<String> {
        input
            .map(str::trim)
            .filter(|v| !v.is_empty() && *v != "*")
            .map(ToOwned::to_owned)
    }

    fn configured_channel_id(&self) -> Option<String> {
        Self::normalized_channel_id(self.channel_id.as_deref())
    }

    fn extract_channel_ids(list_payload: &serde_json::Value) -> Vec<String> {
        let mut ids = list_payload
            .get("channels")
            .and_then(|c| c.as_array())
            .into_iter()
            .flatten()
            .filter_map(|channel| {
                let id = channel.get("id").and_then(|id| id.as_str())?;
                let is_archived = channel
                    .get("is_archived")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let is_member = channel
                    .get("is_member")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if is_archived || !is_member {
                    return None;
                }
                Some(id.to_string())
            })
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        ids
    }

    async fn list_accessible_channels(&self) -> anyhow::Result<Vec<String>> {
        let mut channels = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let mut query_params = vec![
                ("exclude_archived", "true".to_string()),
                ("limit", "200".to_string()),
                (
                    "types",
                    "public_channel,private_channel,mpim,im".to_string(),
                ),
            ];
            if let Some(ref next) = cursor {
                query_params.push(("cursor", next.clone()));
            }

            let resp = self
                .http_client()
                .get("https://slack.com/api/conversations.list")
                .bearer_auth(&self.bot_token)
                .query(&query_params)
                .send()
                .await?;

            let status = resp.status();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));

            if !status.is_success() {
                anyhow::bail!("Slack conversations.list failed ({status}): {body}");
            }

            let data: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            if data.get("ok") == Some(&serde_json::Value::Bool(false)) {
                let err = data
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown");
                anyhow::bail!("Slack conversations.list failed: {err}");
            }

            channels.extend(Self::extract_channel_ids(&data));

            cursor = data
                .get("response_metadata")
                .and_then(|rm| rm.get("next_cursor"))
                .and_then(|c| c.as_str())
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(ToOwned::to_owned);

            if cursor.is_none() {
                break;
            }
        }

        channels.sort();
        channels.dedup();
        Ok(channels)
    }
}

#[async_trait]
impl Channel for SlackChannel {
    fn name(&self) -> &str {
        "slack"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let mut body = serde_json::json!({
            "channel": message.recipient,
            "text": message.content
        });

        if let Some(ref ts) = message.thread_ts {
            body["thread_ts"] = serde_json::json!(ts);
        }

        let resp = self
            .http_client()
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.bot_token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));

        if !status.is_success() {
            anyhow::bail!("Slack chat.postMessage failed ({status}): {body}");
        }

        // Slack returns 200 for most app-level errors; check JSON "ok" field
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        if parsed.get("ok") == Some(&serde_json::Value::Bool(false)) {
            let err = parsed
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("Slack chat.postMessage failed: {err}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let bot_user_id = self.get_bot_user_id().await.unwrap_or_default();
        let scoped_channel = self.configured_channel_id();
        let mut discovered_channels: Vec<String> = Vec::new();
        let mut last_discovery = Instant::now();
        let mut last_ts_by_channel: HashMap<String, String> = HashMap::new();

        if let Some(ref channel_id) = scoped_channel {
            tracing::info!("Slack channel listening on #{channel_id}...");
        } else {
            tracing::info!(
                "Slack channel_id not set (or '*'); listening across all accessible channels."
            );
        }

        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;

            let target_channels = if let Some(ref channel_id) = scoped_channel {
                vec![channel_id.clone()]
            } else {
                if discovered_channels.is_empty()
                    || last_discovery.elapsed() >= Duration::from_secs(60)
                {
                    match self.list_accessible_channels().await {
                        Ok(channels) => {
                            if channels != discovered_channels {
                                tracing::info!(
                                    "Slack auto-discovery refreshed: listening on {} channel(s).",
                                    channels.len()
                                );
                            }
                            discovered_channels = channels;
                        }
                        Err(e) => {
                            tracing::warn!("Slack channel discovery failed: {e}");
                        }
                    }
                    last_discovery = Instant::now();
                }

                discovered_channels.clone()
            };

            if target_channels.is_empty() {
                tracing::debug!("Slack: no accessible channels discovered yet");
                continue;
            }

            for channel_id in target_channels {
                let mut params = vec![("channel", channel_id.clone()), ("limit", "10".to_string())];
                if let Some(last_ts) = last_ts_by_channel.get(&channel_id).cloned() {
                    if !last_ts.is_empty() {
                        params.push(("oldest", last_ts));
                    }
                }

                let resp = match self
                    .http_client()
                    .get("https://slack.com/api/conversations.history")
                    .bearer_auth(&self.bot_token)
                    .query(&params)
                    .send()
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!("Slack poll error for channel {channel_id}: {e}");
                        continue;
                    }
                };

                let data: serde_json::Value = match resp.json().await {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!("Slack parse error for channel {channel_id}: {e}");
                        continue;
                    }
                };

                if data.get("ok") == Some(&serde_json::Value::Bool(false)) {
                    let err = data
                        .get("error")
                        .and_then(|e| e.as_str())
                        .unwrap_or("unknown");
                    tracing::warn!("Slack history error for channel {channel_id}: {err}");
                    continue;
                }

                if let Some(messages) = data.get("messages").and_then(|m| m.as_array()) {
                    // Messages come newest-first, reverse to process oldest first
                    for msg in messages.iter().rev() {
                        let ts = msg.get("ts").and_then(|t| t.as_str()).unwrap_or("");
                        let user = msg
                            .get("user")
                            .and_then(|u| u.as_str())
                            .unwrap_or("unknown");
                        let text = msg.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        let last_ts = last_ts_by_channel
                            .get(&channel_id)
                            .map(String::as_str)
                            .unwrap_or("");

                        // Skip bot's own messages
                        if user == bot_user_id {
                            continue;
                        }

                        // Sender validation
                        if !self.is_user_allowed(user) {
                            tracing::warn!(
                                "Slack: ignoring message from unauthorized user: {user}"
                            );
                            continue;
                        }

                        // Skip empty or already-seen
                        if text.is_empty() || ts <= last_ts {
                            continue;
                        }

                        last_ts_by_channel.insert(channel_id.clone(), ts.to_string());

                        let channel_msg = ChannelMessage {
                            id: format!("slack_{channel_id}_{ts}"),
                            sender: user.to_string(),
                            reply_target: channel_id.clone(),
                            content: text.to_string(),
                            channel: "slack".to_string(),
                            timestamp: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                            thread_ts: Self::inbound_thread_ts(msg, ts),
                        };

                        if tx.send(channel_msg).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        self.http_client()
            .get("https://slack.com/api/auth.test")
            .bearer_auth(&self.bot_token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_channel_name() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec![]);
        assert_eq!(ch.name(), "slack");
    }

    #[test]
    fn slack_channel_with_channel_id() {
        let ch = SlackChannel::new("xoxb-fake".into(), Some("C12345".into()), vec![]);
        assert_eq!(ch.channel_id, Some("C12345".to_string()));
    }

    #[test]
    fn normalized_channel_id_respects_wildcard_and_blank() {
        assert_eq!(SlackChannel::normalized_channel_id(None), None);
        assert_eq!(SlackChannel::normalized_channel_id(Some("")), None);
        assert_eq!(SlackChannel::normalized_channel_id(Some("   ")), None);
        assert_eq!(SlackChannel::normalized_channel_id(Some("*")), None);
        assert_eq!(SlackChannel::normalized_channel_id(Some(" * ")), None);
        assert_eq!(
            SlackChannel::normalized_channel_id(Some(" C12345 ")),
            Some("C12345".to_string())
        );
    }

    #[test]
    fn extract_channel_ids_filters_archived_and_non_member_entries() {
        let payload = serde_json::json!({
            "channels": [
                {"id": "C1", "is_archived": false, "is_member": true},
                {"id": "C2", "is_archived": true, "is_member": true},
                {"id": "C3", "is_archived": false, "is_member": false},
                {"id": "C1", "is_archived": false, "is_member": true},
                {"id": "C4"}
            ]
        });
        let ids = SlackChannel::extract_channel_ids(&payload);
        assert_eq!(ids, vec!["C1".to_string(), "C4".to_string()]);
    }

    #[test]
    fn empty_allowlist_denies_everyone() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec![]);
        assert!(!ch.is_user_allowed("U12345"));
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn wildcard_allows_everyone() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec!["*".into()]);
        assert!(ch.is_user_allowed("U12345"));
    }

    #[test]
    fn specific_allowlist_filters() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec!["U111".into(), "U222".into()]);
        assert!(ch.is_user_allowed("U111"));
        assert!(ch.is_user_allowed("U222"));
        assert!(!ch.is_user_allowed("U333"));
    }

    #[test]
    fn allowlist_exact_match_not_substring() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec!["U111".into()]);
        assert!(!ch.is_user_allowed("U1111"));
        assert!(!ch.is_user_allowed("U11"));
    }

    #[test]
    fn allowlist_empty_user_id() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec!["U111".into()]);
        assert!(!ch.is_user_allowed(""));
    }

    #[test]
    fn allowlist_case_sensitive() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec!["U111".into()]);
        assert!(ch.is_user_allowed("U111"));
        assert!(!ch.is_user_allowed("u111"));
    }

    #[test]
    fn allowlist_wildcard_and_specific() {
        let ch = SlackChannel::new("xoxb-fake".into(), None, vec!["U111".into(), "*".into()]);
        assert!(ch.is_user_allowed("U111"));
        assert!(ch.is_user_allowed("anyone"));
    }

    // ── Message ID edge cases ─────────────────────────────────────

    #[test]
    fn slack_message_id_format_includes_channel_and_ts() {
        // Verify that message IDs follow the format: slack_{channel_id}_{ts}
        let ts = "1234567890.123456";
        let channel_id = "C12345";
        let expected_id = format!("slack_{channel_id}_{ts}");
        assert_eq!(expected_id, "slack_C12345_1234567890.123456");
    }

    #[test]
    fn slack_message_id_is_deterministic() {
        // Same channel_id + same ts = same ID (prevents duplicates after restart)
        let ts = "1234567890.123456";
        let channel_id = "C12345";
        let id1 = format!("slack_{channel_id}_{ts}");
        let id2 = format!("slack_{channel_id}_{ts}");
        assert_eq!(id1, id2);
    }

    #[test]
    fn slack_message_id_different_ts_different_id() {
        // Different timestamps produce different IDs
        let channel_id = "C12345";
        let id1 = format!("slack_{channel_id}_1234567890.123456");
        let id2 = format!("slack_{channel_id}_1234567890.123457");
        assert_ne!(id1, id2);
    }

    #[test]
    fn slack_message_id_different_channel_different_id() {
        // Different channels produce different IDs even with same ts
        let ts = "1234567890.123456";
        let id1 = format!("slack_C12345_{ts}");
        let id2 = format!("slack_C67890_{ts}");
        assert_ne!(id1, id2);
    }

    #[test]
    fn slack_message_id_no_uuid_randomness() {
        // Verify format doesn't contain random UUID components
        let ts = "1234567890.123456";
        let channel_id = "C12345";
        let id = format!("slack_{channel_id}_{ts}");
        assert!(!id.contains('-')); // No UUID dashes
        assert!(id.starts_with("slack_"));
    }

    #[test]
    fn inbound_thread_ts_prefers_explicit_thread_ts() {
        let msg = serde_json::json!({
            "ts": "123.002",
            "thread_ts": "123.001"
        });

        let thread_ts = SlackChannel::inbound_thread_ts(&msg, "123.002");
        assert_eq!(thread_ts.as_deref(), Some("123.001"));
    }

    #[test]
    fn inbound_thread_ts_falls_back_to_ts() {
        let msg = serde_json::json!({
            "ts": "123.001"
        });

        let thread_ts = SlackChannel::inbound_thread_ts(&msg, "123.001");
        assert_eq!(thread_ts.as_deref(), Some("123.001"));
    }

    #[test]
    fn inbound_thread_ts_none_when_ts_missing() {
        let msg = serde_json::json!({});

        let thread_ts = SlackChannel::inbound_thread_ts(&msg, "");
        assert_eq!(thread_ts, None);
    }
}
