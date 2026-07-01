use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, RwLock};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::{StreamMode, default_dingtalk_streaming_interval_ms};

const DINGTALK_BOT_CALLBACK_TOPIC: &str = "/v1.0/im/bot/messages/get";

/// DingTalk channel — connects via Stream Mode WebSocket for real-time messages.
/// Replies are sent through per-message session webhook URLs.
pub struct DingTalkChannel {
    client_id: String,
    client_secret: String,
    /// The alias key under `[channels.dingtalk.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// Per-chat session webhooks for sending replies (chatID -> webhook URL).
    /// DingTalk provides a unique webhook URL with each incoming message.
    session_webhooks: Arc<RwLock<HashMap<String, String>>>,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    /// Streaming mode for AI card responses (off/partial).
    stream_mode: StreamMode,
    /// Minimum interval between streamingUpdate calls in milliseconds.
    streaming_update_interval_ms: u64,
    /// Per-card timestamp of the last streamingUpdate PUT.
    last_streaming_edit: Arc<Mutex<HashMap<String, Instant>>>,
    /// Per-card buffer of the latest accumulated text waiting to be flushed.
    pending_streaming_text: Arc<Mutex<HashMap<String, String>>>,
    /// Cache of active AI card instances (cardInstanceId -> recipient).
    card_instances: Arc<RwLock<HashMap<String, DingTalkCardInstance>>>,
    /// Per-card notifier fired once `deliver` has completed. `streamingUpdate`
    /// PUTs await the receiver before sending — without this, an early PUT
    /// races a still-in-flight deliver and DingTalk rejects it with a
    /// `cardInstanceId not found` logical error. The entry is removed after
    /// delivery so the map only carries in-flight cards.
    pending_deliver: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Receiver<()>>>>,
    /// AI Card Template ID for streaming responses.
    ai_card_template_id: Option<String>,
}

/// AI card instance information for tracking active streaming sessions.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct DingTalkCardInstance {
    card_instance_id: String,
    created_at: Instant,
    recipient: String,
}

/// Lightweight handle to drive `deliver_ai_card` from a background task.
/// The full `DingTalkChannel` is not `Clone` (it owns a non-cloneable trait
/// object resolver), but every field needed for deliver is cheap to clone
/// (Arc-wrapped or String). Used only by `send_ai_card` after spawning the
/// deliver task.
struct DeliverHandle {
    client_id: String,
    client_secret: String,
    proxy_url: Option<String>,
    pending_deliver: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Receiver<()>>>>,
}

impl DeliverHandle {
    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client(
            "channel.dingtalk",
            self.proxy_url.as_deref(),
        )
    }

    /// Same heuristic as `DingTalkChannel::reply_target_for_recipient` so
    /// the deliver path picks the right openSpaceId. Single-chat staffId
    /// (all-digit, no `group`/`conversation` marker) → IM_ROBOT; otherwise
    /// → IM_GROUP.
    fn is_group_recipient(recipient: &str) -> bool {
        let lower = recipient.to_ascii_lowercase();
        lower.contains("group")
            || lower.contains("conversation")
            || (lower.starts_with("c")
                && recipient.chars().filter(|c| c.is_ascii_digit()).count() > 12)
    }

    async fn deliver_ai_card(&self, out_track_id: &str, recipient: &str) -> anyhow::Result<()> {
        // Mirrors the production deliver path on DingTalkChannel; copies
        // the body shape but skips the channel-state cache write since the
        // background task only logs the outcome. Group vs. single chat is
        // decided with the same heuristic as the channel impl.
        let is_group = Self::is_group_recipient(recipient);
        let (open_space_id, deliver_model) = if is_group {
            (
                format!("dtv1.card//IM_GROUP.{}", recipient),
                serde_json::json!({ "robotCode": self.client_id }),
            )
        } else {
            (
                format!("dtv1.card//IM_ROBOT.{}", recipient),
                serde_json::json!({ "spaceType": "IM_ROBOT" }),
            )
        };
        let body = serde_json::json!({
            "outTrackId": out_track_id,
            "userIdType": 1,
            "openSpaceId": open_space_id,
            "imGroupOpenDeliverModel": deliver_model.clone(),
            "imRobotOpenDeliverModel": deliver_model,
        });
        let token_resp = self
            .http_client()
            .post("https://api.dingtalk.com/v1.0/oauth2/accessToken")
            .json(&serde_json::json!({
                "appKey": self.client_id,
                "appSecret": self.client_secret,
            }))
            .send()
            .await?;
        let token_body: serde_json::Value = token_resp.json().await?;
        let token = token_body
            .get("accessToken")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::Error::msg("DingTalk: accessToken missing"))?
            .to_string();

        let resp = self
            .http_client()
            .post("https://api.dingtalk.com/v1.0/card/instances/deliver")
            .header("x-acs-dingtalk-access-token", &token)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            zeroclaw_log::record!(
                WARN,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                    serde_json::json!({
                        "out_track_id": out_track_id,
                        "error": err,
                    })
                ),
                "DingTalk: AI card deliver failed"
            );
            anyhow::bail!("deliver failed ({status}): {err}");
        }

        let body_text = resp.text().await.unwrap_or_default();
        zeroclaw_log::record!(
            INFO,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                serde_json::json!({
                    "out_track_id": out_track_id,
                    "response_body": body_text,
                })
            ),
            "DingTalk: AI card delivered"
        );
        Ok(())
    }
}

/// Response from DingTalk gateway connection registration.
#[derive(serde::Deserialize)]
struct GatewayResponse {
    endpoint: String,
    ticket: String,
}

impl DingTalkChannel {
    pub fn new(
        client_id: String,
        client_secret: String,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self {
            client_id,
            client_secret,
            alias: alias.into(),
            peer_resolver,
            session_webhooks: Arc::new(RwLock::new(HashMap::new())),
            proxy_url: None,
            stream_mode: StreamMode::default(),
            streaming_update_interval_ms: default_dingtalk_streaming_interval_ms(),
            last_streaming_edit: Arc::new(Mutex::new(HashMap::new())),
            pending_streaming_text: Arc::new(Mutex::new(HashMap::new())),
            card_instances: Arc::new(RwLock::new(HashMap::new())),
            pending_deliver: Arc::new(Mutex::new(HashMap::new())),
            ai_card_template_id: None,
        }
    }

    /// Return the alias under `[channels.dingtalk.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Set a per-channel proxy URL that overrides the global proxy config.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    /// Set the AI card template ID for streaming responses.
    pub fn with_ai_card_template(mut self, template_id: String) -> Self {
        self.ai_card_template_id = Some(template_id);
        self
    }

    /// Set the AI card template ID for streaming responses (optional).
    pub fn with_ai_card_template_opt(mut self, template_id: Option<String>) -> Self {
        self.ai_card_template_id = template_id;
        self
    }

    /// Configure progressive AI card streaming.
    pub fn with_streaming(mut self, stream_mode: StreamMode, update_interval_ms: u64) -> Self {
        let effective_stream_mode = match stream_mode {
            StreamMode::MultiMessage => {
                zeroclaw_log::record!(
                    WARN,
                    zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                        .with_attrs(serde_json::json!({"requested_mode": "multi_message"})),
                    "DingTalk: stream_mode=multi_message is not supported; falling back to off"
                );
                StreamMode::Off
            }
            mode => mode,
        };
        self.stream_mode = effective_stream_mode;
        self.streaming_update_interval_ms = update_interval_ms;
        self
    }

    /// Check if streaming mode is enabled.
    fn supports_streaming(&self) -> bool {
        // Streaming requires both Partial mode AND a configured AI card template ID
        matches!(self.stream_mode, StreamMode::Partial) && self.ai_card_template_id.is_some()
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client(
            "channel.dingtalk",
            self.proxy_url.as_deref(),
        )
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_user_allowed(&peers, user_id, crate::allowlist::Match::Sensitive)
    }

    fn parse_stream_data(frame: &serde_json::Value) -> Option<serde_json::Value> {
        match frame.get("data") {
            Some(serde_json::Value::String(raw)) => serde_json::from_str(raw).ok(),
            Some(serde_json::Value::Object(_)) => frame.get("data").cloned(),
            _ => None,
        }
    }

    fn resolve_chat_id(data: &serde_json::Value, sender_id: &str) -> String {
        let is_private_chat = data
            .get("conversationType")
            .and_then(|value| {
                value
                    .as_str()
                    .map(|v| v == "1")
                    .or_else(|| value.as_i64().map(|v| v == 1))
            })
            .unwrap_or(true);

        if is_private_chat {
            sender_id.to_string()
        } else {
            data.get("conversationId")
                .and_then(|c| c.as_str())
                .unwrap_or(sender_id)
                .to_string()
        }
    }

    /// Register a connection with DingTalk's gateway to get a WebSocket endpoint.
    async fn register_connection(&self) -> anyhow::Result<GatewayResponse> {
        let body = serde_json::json!({
            "clientId": self.client_id,
            "clientSecret": self.client_secret,
            "subscriptions": [
                {
                    "type": "CALLBACK",
                    "topic": DINGTALK_BOT_CALLBACK_TOPIC,
                }
            ],
        });

        let resp = self
            .http_client()
            .post("https://api.dingtalk.com/v1.0/gateway/connections/open")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("gateway registration failed ({status}): {err}");
        }

        let gw: GatewayResponse = resp.json().await?;
        Ok(gw)
    }

    /// Get access token for DingTalk API calls
    async fn get_access_token(&self) -> anyhow::Result<String> {
        let body = serde_json::json!({
            "appKey": self.client_id,
            "appSecret": self.client_secret,
        });

        let resp = self
            .http_client()
            .post("https://api.dingtalk.com/v1.0/oauth2/accessToken")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("token request failed ({status}): {err}");
        }

        let token_resp: serde_json::Value = resp.json().await?;
        token_resp
            .get("accessToken")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::Error::msg("accessToken not found in response"))
    }

    /// Determine the reply target for a recipient (single chat or group).
    ///
    /// Bug fix vs. the original heuristic: the previous implementation
    /// classified any all-numeric recipient as a group chat, but DingTalk
    /// 1:1 `sender_staff_id` values are also all-numeric (e.g.
    /// `0364501605471227014`). That mis-routing fed the `imGroupOpenSpaceModel`
    /// and `IM_GROUP` openSpaceId into the create + deliver calls, which the
    /// DingTalk card platform then silently rejected with `success: false`
    /// under HTTP 200 — the user saw nothing.
    ///
    /// Conservative fix: default to `Single` for the typical 1:1 staffId
    /// shape (≤32 digits). Only return `Group` when the recipient string
    /// carries an unambiguous group marker.
    async fn reply_target_for_recipient(&self, recipient: &str) -> Option<DingTalkReplyTarget> {
        let lower = recipient.to_ascii_lowercase();
        let looks_like_group = lower.contains("group")
            || lower.contains("conversation")
            || lower.starts_with("c")
                && recipient.chars().filter(|c| c.is_ascii_digit()).count() > 12;
        if looks_like_group {
            Some(DingTalkReplyTarget::Group(recipient.to_string()))
        } else {
            Some(DingTalkReplyTarget::Single(recipient.to_string()))
        }
    }

    /// Create an AI card instance for streaming responses and return its
    /// `outTrackId`. Subsequent `streamingUpdate` calls reference this same id.
    async fn send_ai_card(&self, recipient: &str, initial_content: &str) -> anyhow::Result<String> {
        let template_id = self
            .ai_card_template_id
            .as_ref()
            .ok_or_else(|| anyhow::Error::msg("AI card template ID not configured."))?;

        let out_track_id = Uuid::new_v4().to_string();
        let token = self.get_access_token().await?;

        // Determine if this is a group or single chat
        let is_group = matches!(
            self.reply_target_for_recipient(recipient).await,
            Some(DingTalkReplyTarget::Group(_))
        );

        let mut create_body = serde_json::json!({
            "cardTemplateId": template_id,
            "outTrackId": out_track_id,
            "callbackType": "STREAM",
            "cardData": {
                "cardParamMap": {
                    "content": initial_content,
                    "status": "thinking",
                },
            },
        });

        // Add space model based on single/group chat
        if is_group {
            create_body.as_object_mut().unwrap().insert(
                "imGroupOpenSpaceModel".into(),
                serde_json::json!({
                    "supportForward": true,
                    "openSpaceId": recipient,
                    "robotCode": self.client_id,
                }),
            );
        } else {
            create_body.as_object_mut().unwrap().insert(
                "imRobotOpenSpaceModel".into(),
                serde_json::json!({
                    "supportForward": true,
                    "robotCode": self.client_id,
                }),
            );
        }

        // POST /v1.0/card/instances
        let resp = self
            .http_client()
            .post("https://api.dingtalk.com/v1.0/card/instances")
            .header("x-acs-dingtalk-access-token", &token)
            .json(&create_body)
            .send()
            .await?;

        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("create card instance failed ({}): {}", status, body_text);
        }

        // DingTalk returns `{"success": false, "errorMsg": "..."}` for
        // logical errors (e.g. invalid cardTemplateId, missing key,
        // permission denied) with HTTP 200. Surface the failure here
        // instead of blindly logging "created successfully".
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body_text)
            && parsed.get("success").and_then(serde_json::Value::as_bool) == Some(false)
        {
            let err_msg = parsed
                .get("errorMsg")
                .or_else(|| parsed.get("errorMessage"))
                .or_else(|| parsed.get("message"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let err_code = parsed
                .get("errorCode")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            zeroclaw_log::record!(
                ERROR,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Reject)
                    .with_outcome(zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(serde_json::json!({
                        "out_track_id": out_track_id,
                        "card_template_id": template_id,
                        "error_code": err_code,
                        "error_msg": err_msg,
                    })),
                "DingTalk: AI card create returned success=false"
            );
            anyhow::bail!(
                "create card instance failed (success=false, code={}): {}",
                err_code,
                err_msg
            );
        }

        // Cache the card instance immediately so draft updater can resolve
        // it for streamingUpdate.
        {
            let mut instances = self.card_instances.write().await;
            instances.insert(
                out_track_id.clone(),
                DingTalkCardInstance {
                    card_instance_id: out_track_id.clone(),
                    created_at: Instant::now(),
                    recipient: recipient.to_string(),
                },
            );
        }

        // Fire-and-forget deliver: deliver is required to surface the card
        // to the recipient's chat, but waiting on it here would block the
        // orchestrator for ~1s after create before the LLM call can start.
        // By spawning deliver concurrently, the LLM call and the deliver
        // POST overlap. The draft updater (`streaming_update_card`) blocks
        // on the receiver below before its first PUT, so the user never
        // sees a "card not found" error from an early streamingUpdate.
        //
        // The Receiver is what the `await_pending_deliver` future awaits;
        // the Sender is owned by the background deliver task and fires
        // `()` when deliver completes (or is dropped on error).
        let (deliver_tx, deliver_rx) = tokio::sync::oneshot::channel::<()>();
        {
            let mut pending = self.pending_deliver.lock().await;
            pending.insert(out_track_id.clone(), deliver_rx);
        }
        let deliver_handle = self.clone_for_deliver_spawn();
        let out_track_id_for_deliver = out_track_id.clone();
        let recipient_for_deliver = recipient.to_string();
        zeroclaw_spawn::spawn!(async move {
            let deliver_start = std::time::Instant::now();
            let result = deliver_handle
                .deliver_ai_card(&out_track_id_for_deliver, &recipient_for_deliver)
                .await;
            let deliver_elapsed_ms =
                u64::try_from(deliver_start.elapsed().as_millis()).unwrap_or(u64::MAX);
            // Pop the receiver first; if no one is waiting (deliver
            // completed before the first PUT), the entry is already gone
            // and `tx.send(())` here would fail. The send always fires
            // regardless — a Receiver that was already removed is
            // simply dropped, which terminates the await in
            // `await_pending_deliver` with `Err(_)` and is treated as
            // "deliver done" upstream.
            let _rx = deliver_handle
                .pending_deliver
                .lock()
                .await
                .remove(&out_track_id_for_deliver);
            // Signal any waiter. `send` is infallible from the Sender's
            // perspective; the Receiver is closed if the waiter is gone.
            let _ = deliver_tx.send(());
            zeroclaw_log::record!(
                INFO,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                    serde_json::json!({
                        "out_track_id": out_track_id_for_deliver,
                        "ok": result.is_ok(),
                        "deliver_elapsed_ms": deliver_elapsed_ms,
                    })
                ),
                "DingTalk: AI card deliver task complete"
            );
        });

        Ok(out_track_id)
    }

    /// Build a shallow-clone handle that can drive `deliver_ai_card` from a
    /// background task. The `DingTalkChannel` itself is not `Clone` (the
    /// `peer_resolver` is a non-cloneable trait object), but every relevant
    /// field is `Arc`/cheap-to-clone, so we hand-roll a structural clone.
    fn clone_for_deliver_spawn(&self) -> DeliverHandle {
        DeliverHandle {
            client_id: self.client_id.clone(),
            client_secret: self.client_secret.clone(),
            proxy_url: self.proxy_url.clone(),
            pending_deliver: Arc::clone(&self.pending_deliver),
        }
    }

    /// Deliver an AI card to the recipient's chat. The production deliver
    /// path is driven by `DeliverHandle` from a background task in
    /// `send_ai_card`; this impl is kept for potential future callers
    /// (e.g. explicit redeliver on cancel) and mirrors the DeliverHandle
    /// behavior.
    #[allow(dead_code)]
    async fn deliver_ai_card(&self, out_track_id: &str, recipient: &str) -> anyhow::Result<()> {
        let token = self.get_access_token().await?;
        let is_group = matches!(
            self.reply_target_for_recipient(recipient).await,
            Some(DingTalkReplyTarget::Group(_))
        );

        // Build openSpaceId (DingTalk routing identifier)
        let (open_space_id, deliver_model) = if is_group {
            (
                format!("dtv1.card//IM_GROUP.{}", recipient),
                serde_json::json!({
                    "robotCode": self.client_id,
                }),
            )
        } else {
            (
                format!("dtv1.card//IM_ROBOT.{}", recipient),
                serde_json::json!({
                    "spaceType": "IM_ROBOT",
                }),
            )
        };

        let body = serde_json::json!({
            "outTrackId": out_track_id,
            "userIdType": 1,
            "openSpaceId": open_space_id,
            "imGroupOpenDeliverModel": deliver_model.clone(),
            "imRobotOpenDeliverModel": deliver_model,
        });

        // POST /v1.0/card/instances/deliver
        let resp = self
            .http_client()
            .post("https://api.dingtalk.com/v1.0/card/instances/deliver")
            .header("x-acs-dingtalk-access-token", &token)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let _status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            zeroclaw_log::record!(
                WARN,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                    serde_json::json!({
                        "out_track_id": out_track_id,
                        "error": err,
                    })
                ),
                "DingTalk: AI card deliver failed"
            );
            return Ok(()); // Non-fatal, continue
        }

        let deliver_body = resp.text().await.unwrap_or_default();
        zeroclaw_log::record!(
            INFO,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                serde_json::json!({
                    "out_track_id": out_track_id,
                    "response_body": deliver_body,
                })
            ),
            "DingTalk: AI card delivered"
        );

        Ok(())
    }

    /// Block until the background deliver task for `out_track_id` has
    /// fired, or `timeout` elapses, whichever comes first. Called from
    /// `streaming_update_card` before the first PUT to ensure the card is
    /// actually deliverable; subsequent PUTs skip this (the entry has
    /// already been removed by the deliver task). Returns `true` if the
    /// deliver is done (or never started), `false` on timeout.
    async fn await_pending_deliver(
        &self,
        out_track_id: &str,
        timeout: std::time::Duration,
    ) -> bool {
        // Take the receiver out of the map; if there is none, deliver has
        // already completed (or never started) and we can return true.
        let rx = {
            let mut pending = self.pending_deliver.lock().await;
            pending.remove(out_track_id)
        };
        let Some(rx) = rx else {
            return true;
        };
        let wait_start = std::time::Instant::now();
        let outcome = tokio::time::timeout(timeout, rx).await;
        let waited_ms = u64::try_from(wait_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let completed = matches!(outcome, Ok(Ok(())));
        zeroclaw_log::record!(
            DEBUG,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                serde_json::json!({
                    "out_track_id": out_track_id,
                    "deliver_wait_ms": waited_ms,
                    "deliver_completed": completed,
                })
            ),
            "DingTalk: deliver wait"
        );
        completed
    }

    /// Push a streaming update to a previously created AI card.
    async fn streaming_update_card(
        &self,
        card_instance_id: &str,
        content: &str,
        is_final: bool,
    ) -> anyhow::Result<()> {
        // Block until the background `deliver` task has fired (or 2s
        // elapses). Without this, an early PUT can race the deliver POST
        // and DingTalk rejects it with `cardInstanceId not found`. The wait
        // is best-effort: if deliver is slow, the PUT still goes out and
        // the gateway either accepts it (deliver finished in the meantime)
        // or returns a logical error that the caller will surface.
        let _delivered = self
            .await_pending_deliver(card_instance_id, std::time::Duration::from_millis(2000))
            .await;

        let token = self.get_access_token().await?;
        let guid = Uuid::new_v4().to_string();

        // Per the DingTalk `streamingUpdate` contract
        // (https://open.dingtalk.com/document/development/api-streamingupdate)
        // and the official `dingtalk-stream` Python SDK
        // (`AICardReplier.async_streaming`):
        //   * `content` is a plain string, NOT a pre-JSON-encoded value.
        //     Pre-encoding once (the previous behavior of this site) put a
        //     JSON-quoted string into `content`, which the gateway accepted
        //     with HTTP 200 but the card template never re-rendered, leaving
        //     users stuck on the "thinking..." placeholder.
        //   * `isFull: true` means the body is the full accumulated text,
        //     not a delta. The orchestrator already passes the full
        //     accumulated buffer on every update, so we always send `true`.
        //   * `isFinalize: true` closes the card (triggers the "Done" reaction).
        let body = serde_json::json!({
            "outTrackId": card_instance_id,
            "guid": guid,
            "key": "content",
            "content": content,
            "isFull": true,
            "isFinalize": is_final,
            "isError": false,
        });

        // PUT /v1.0/card/streaming
        let resp = self
            .http_client()
            .put("https://api.dingtalk.com/v1.0/card/streaming")
            .header("x-acs-dingtalk-access-token", &token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "card_instance_id": card_instance_id,
                    "is_final": is_final,
                    "http_status": status.as_u16(),
                    "response_body": body_text,
                    "request_body": body,
                })
            ),
            "DingTalk: streamingUpdate HTTP response"
        );
        if !status.is_success() {
            anyhow::bail!("streaming update failed ({status}): {body_text}");
        }

        // DingTalk returns `{"success": false, "errorMsg": "..."}` for
        // logical errors (e.g. unknown outTrackId, missing key) with
        // HTTP 200. Surface the failure so the orchestrator can fall
        // back to a non-streaming send instead of silently dropping the
        // draft.
        let logical_error: Option<String> =
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body_text) {
                if parsed.get("success").and_then(serde_json::Value::as_bool) == Some(false) {
                    let err_msg = parsed
                        .get("errorMsg")
                        .or_else(|| parsed.get("errorMessage"))
                        .or_else(|| parsed.get("message"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown")
                        .to_string();
                    let err_code = parsed
                        .get("errorCode")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Some(format!("{err_code}: {err_msg}"))
                } else {
                    None
                }
            } else {
                None
            };

        if let Some(err) = logical_error.clone() {
            zeroclaw_log::record!(
                ERROR,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Reject)
                    .with_outcome(zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(serde_json::json!({
                        "card_instance_id": card_instance_id,
                        "is_final": is_final,
                        "error": err,
                        "response_body": body_text,
                    })),
                "DingTalk: streamingUpdate returned success=false"
            );
            anyhow::bail!("streaming update logical failure: {err}");
        }

        // Cleanup completed card instances
        if is_final {
            let mut instances = self.card_instances.write().await;
            instances.remove(card_instance_id);
        }

        Ok(())
    }
}

/// Reply target for DingTalk (single chat or group)
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum DingTalkReplyTarget {
    Single(String),
    Group(String),
}

impl ::zeroclaw_api::attribution::Attributable for DingTalkChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::DingTalk,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for DingTalkChannel {
    fn name(&self) -> &str {
        "dingtalk"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let webhooks = self.session_webhooks.read().await;
        let webhook_url = webhooks.get(&message.recipient).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "recipient": message.recipient,
                        "reason": "no_session_webhook",
                    })),
                "dingtalk: no session webhook for recipient"
            );
            anyhow::Error::msg(format!(
                "No session webhook found for chat {}. \
                 The user must send a message first to establish a session.",
                message.recipient
            ))
        })?;

        let title = message.subject.as_deref().unwrap_or("ZeroClaw");
        let body = serde_json::json!({
            "msgtype": "markdown",
            "markdown": {
                "title": title,
                "text": message.content,
            }
        });

        let resp = self
            .http_client()
            .post(webhook_url)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("webhook reply failed ({status}): {err}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "registering gateway connection..."
        );

        let gw = self.register_connection().await?;
        let ws_url = format!("{}?ticket={}", gw.endpoint, gw.ticket);

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "connecting to stream WebSocket..."
        );
        let (ws_stream, _) = zeroclaw_config::schema::ws_connect_with_proxy(
            &ws_url,
            "channel.dingtalk",
            self.proxy_url.as_deref(),
        )
        .await?;
        let (mut write, mut read) = ws_stream.split();

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "connected and listening for messages..."
        );

        while let Some(msg) = read.next().await {
            let msg = match msg {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "WebSocket error"
                    );
                    break;
                }
                _ => continue,
            };

            let frame: serde_json::Value = match serde_json::from_str(msg.as_ref()) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let frame_type = frame.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match frame_type {
                "SYSTEM" => {
                    // Respond to system pings to keep the connection alive
                    let message_id = frame
                        .get("headers")
                        .and_then(|h| h.get("messageId"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("");

                    let pong = serde_json::json!({
                        "code": 200,
                        "headers": {
                            "contentType": "application/json",
                            "messageId": message_id,
                        },
                        "message": "OK",
                        "data": "",
                    });

                    if let Err(e) = write.send(Message::Text(pong.to_string().into())).await {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "failed to send pong"
                        );
                        break;
                    }
                }
                "EVENT" | "CALLBACK" => {
                    // Parse the chatbot callback data from the frame.
                    let data = match Self::parse_stream_data(&frame) {
                        Some(v) => v,
                        None => {
                            ::zeroclaw_log::record!(
                                DEBUG,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                ),
                                "frame has no parseable data payload"
                            );
                            continue;
                        }
                    };

                    // Extract message content
                    let content = data
                        .get("text")
                        .and_then(|t| t.get("content"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .trim();

                    if content.is_empty() {
                        continue;
                    }

                    let sender_id = data
                        .get("senderStaffId")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown");

                    if !self.is_user_allowed(sender_id) {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"sender_id": sender_id})),
                            "ignoring message from unauthorized user"
                        );
                        continue;
                    }

                    // Private chat uses sender ID, group chat uses conversation ID.
                    let chat_id = Self::resolve_chat_id(&data, sender_id);

                    // Store session webhook for later replies
                    if let Some(webhook) = data.get("sessionWebhook").and_then(|w| w.as_str()) {
                        let webhook = webhook.to_string();
                        let mut webhooks = self.session_webhooks.write().await;
                        // Use both keys so reply routing works for both group and private flows.
                        webhooks.insert(chat_id.clone(), webhook.clone());
                        webhooks.insert(sender_id.to_string(), webhook);
                    }

                    // Acknowledge the event
                    let message_id = frame
                        .get("headers")
                        .and_then(|h| h.get("messageId"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("");

                    let ack = serde_json::json!({
                        "code": 200,
                        "headers": {
                            "contentType": "application/json",
                            "messageId": message_id,
                        },
                        "message": "OK",
                        "data": "",
                    });
                    let _ = write.send(Message::Text(ack.to_string().into())).await;

                    let channel_msg = ChannelMessage {
                        id: Uuid::new_v4().to_string(),
                        sender: sender_id.to_string(),
                        reply_target: chat_id,
                        content: content.to_string(),
                        channel: "dingtalk".to_string(),
                        channel_alias: Some(self.alias.clone()),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                        thread_ts: None,
                        interruption_scope_id: None,
                        attachments: vec![],
                        subject: None,

                        ..Default::default()
                    };

                    if tx.send(channel_msg).await.is_err() {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                            "message channel closed"
                        );
                        break;
                    }
                }
                _ => {}
            }
        }

        anyhow::bail!("WebSocket stream ended")
    }

    async fn health_check(&self) -> bool {
        self.register_connection().await.is_ok()
    }

    /// True when both Partial mode is enabled and a template id is set.
    fn supports_draft_updates(&self) -> bool {
        self.supports_streaming()
    }

    /// Open a streaming AI card for the recipient.
    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        if !self.supports_streaming() {
            return Ok(None); // Fallback to non-streaming send()
        }
        match self.send_ai_card(&message.recipient, "正在思考中…").await {
            Ok(card_id) => {
                zeroclaw_log::record!(
                    INFO,
                    zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                        .with_attrs(serde_json::json!({
                            "card_id": card_id,
                            "recipient": &message.recipient,
                        })),
                    "DingTalk: send_draft opened streaming card"
                );
                Ok(Some(card_id))
            }
            Err(error) => {
                zeroclaw_log::record!(
                    WARN,
                    zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                        .with_attrs(serde_json::json!({
                            "recipient": &message.recipient,
                            "error": format!("{error}"),
                        })),
                    "DingTalk: send_draft failed, falling back to non-streaming send()"
                );
                Ok(None)
            }
        }
    }

    /// Push an incremental AI card update with the latest accumulated text.
    async fn update_draft(
        &self,
        _recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        zeroclaw_log::record!(
            DEBUG,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                serde_json::json!({
                    "message_id": message_id,
                    "text_bytes": text.len(),
                    "supports_streaming": self.supports_streaming(),
                })
            ),
            "DingTalk: update_draft entry"
        );
        if message_id.is_empty() || !self.supports_streaming() {
            return Ok(());
        }

        let interval_ms = self.streaming_update_interval_ms;
        let now = Instant::now();

        // Throttle: drop intermediate deltas inside the cooldown window
        // (matches Lark's `update_draft`). Inside-window calls return
        // immediately without cache or PUT — the orchestrator already
        // passes the full accumulated buffer, so the next call outside
        // the window carries the freshest content. This makes the
        // throttle a real rate-limiter instead of cache-and-flush, so
        // PUTs fire at the configured cadence even when the upstream
        // LLM token rate is faster than the network RTT.
        {
            let last_guard = self.last_streaming_edit.lock().await;
            if last_guard
                .get(message_id)
                .map(|last| {
                    u64::try_from(now.duration_since(*last).as_millis()).unwrap_or(u64::MAX)
                })
                .is_some_and(|elapsed| elapsed < interval_ms)
            {
                return Ok(());
            }
        }
        {
            let mut last_guard = self.last_streaming_edit.lock().await;
            last_guard.insert(message_id.to_string(), now);
        }

        zeroclaw_log::record!(
            DEBUG,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                serde_json::json!({
                    "message_id": message_id,
                    "to_send_bytes": text.len(),
                })
            ),
            "DingTalk: update_draft -> streaming_update_card"
        );
        if let Err(error) = self.streaming_update_card(message_id, text, false).await {
            zeroclaw_log::record!(
                WARN,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                    .with_attrs(serde_json::json!({"error": format!("{}", error)})),
                "DingTalk: update_draft streaming call failed"
            );
        }
        Ok(())
    }

    /// Close the AI card with the final accumulated text.
    async fn finalize_draft(
        &self,
        _recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        zeroclaw_log::record!(
            DEBUG,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                serde_json::json!({
                    "message_id": message_id,
                    "text_bytes": text.len(),
                    "supports_streaming": self.supports_streaming(),
                })
            ),
            "DingTalk: finalize_draft entry"
        );
        if message_id.is_empty() || !self.supports_streaming() {
            return Ok(());
        }

        // Flush any cached text first (in case we're inside throttle window)
        let cached_text = {
            let mut cache = self.pending_streaming_text.lock().await;
            cache.remove(&message_id.to_string())
        };

        // Use cached text if available, otherwise use provided text
        let used_cached = cached_text.is_some();
        let final_text = cached_text.unwrap_or_else(|| text.to_string());

        zeroclaw_log::record!(
            INFO,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note).with_attrs(
                serde_json::json!({
                    "message_id": message_id,
                    "used_cached": used_cached,
                    "final_text_bytes": final_text.len(),
                })
            ),
            "DingTalk: finalize_draft -> streaming_update_card"
        );

        // Cleanup timestamp state
        self.last_streaming_edit
            .lock()
            .await
            .remove(&message_id.to_string());

        if let Err(error) = self
            .streaming_update_card(message_id, &final_text, true)
            .await
        {
            zeroclaw_log::record!(
                WARN,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note)
                    .with_attrs(serde_json::json!({"error": format!("{}", error)})),
                "DingTalk: finalize_draft streaming call failed"
            );
            return Err(error);
        }
        Ok(())
    }

    /// Best-effort cancel: send a final update with a notice.
    async fn cancel_draft(&self, _recipient: &str, message_id: &str) -> anyhow::Result<()> {
        if message_id.is_empty() || !self.supports_streaming() {
            return Ok(());
        }

        let result = self
            .streaming_update_card(message_id, "[回答已取消]", true)
            .await;
        self.last_streaming_edit
            .lock()
            .await
            .remove(&message_id.to_string());
        self.pending_streaming_text
            .lock()
            .await
            .remove(&message_id.to_string());

        if let Err(_error) = result {
            zeroclaw_log::record!(
                WARN,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
                "DingTalk: cancel_draft streaming call failed"
            );
        }
        Ok(())
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // No typing-indicator API in the DingTalk Open Platform.
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn is_direct_message(&self, msg: &zeroclaw_api::channel::ChannelMessage) -> bool {
        // DingTalk Stream Mode: sender ID equals reply_target means private chat
        // Group chats have different conversation IDs
        msg.sender == msg.reply_target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        );
        assert_eq!(ch.name(), "dingtalk");
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_user_allowed_specific() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(|| vec!["user123".into()]),
        );
        assert!(ch.is_user_allowed("user123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn test_user_denied_empty() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        );
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn v2_allowed_users_fold_into_peer_groups() {
        // V2 `[channels.dingtalk].allowed_users` migrates into a synthesized
        // `[peer_groups.dingtalk_default]` block in V3. The wildcard sentinel
        // is filtered out during synthesis so only concrete usernames survive
        // as external peers.
        let v2_toml = r#"
schema_version = 2

[channels.dingtalk]
enabled = true
client_id = "app_id_123"
client_secret = "secret_456"
allowed_users = ["user1", "*"]
"#;
        let cfg = zeroclaw_config::migration::migrate_to_current(v2_toml)
            .expect("V2 dingtalk config migrates to V3");
        let dingtalk = cfg
            .channels
            .dingtalk
            .get("default")
            .expect("V2 dingtalk folds under alias `default`");
        assert_eq!(dingtalk.client_id, "app_id_123");
        assert_eq!(dingtalk.client_secret, "secret_456");

        let group = cfg
            .peer_groups
            .get("dingtalk_default")
            .expect("dingtalk allow-list synthesizes [peer_groups.dingtalk_default]");
        assert_eq!(group.channel, "dingtalk");
        let peers: Vec<&str> = group.external_peers.iter().map(|p| p.as_str()).collect();
        assert_eq!(peers, vec!["user1"]);
    }

    #[test]
    fn v2_no_allowed_users_synthesizes_no_peer_group() {
        // V2 dingtalk without `allowed_users` must not synthesize a peer group;
        // V3 leaves `peer_groups` empty rather than emitting an empty block.
        let v2_toml = r#"
schema_version = 2

[channels.dingtalk]
enabled = true
client_id = "id"
client_secret = "secret"
"#;
        let cfg = zeroclaw_config::migration::migrate_to_current(v2_toml)
            .expect("V2 dingtalk config without allowed_users migrates");
        assert!(
            !cfg.peer_groups.contains_key("dingtalk_default"),
            "no peer group synthesized when allowed_users is absent"
        );
    }

    #[test]
    fn parse_stream_data_supports_string_payload() {
        let frame = serde_json::json!({
            "data": "{\"text\":{\"content\":\"hello\"}}"
        });
        let parsed = DingTalkChannel::parse_stream_data(&frame).unwrap();
        assert_eq!(
            parsed.get("text").and_then(|v| v.get("content")),
            Some(&serde_json::json!("hello"))
        );
    }

    #[test]
    fn parse_stream_data_supports_object_payload() {
        let frame = serde_json::json!({
            "data": {"text": {"content": "hello"}}
        });
        let parsed = DingTalkChannel::parse_stream_data(&frame).unwrap();
        assert_eq!(
            parsed.get("text").and_then(|v| v.get("content")),
            Some(&serde_json::json!("hello"))
        );
    }

    #[test]
    fn resolve_chat_id_handles_numeric_group_conversation_type() {
        let data = serde_json::json!({
            "conversationType": 2,
            "conversationId": "cid-group",
        });
        let chat_id = DingTalkChannel::resolve_chat_id(&data, "staff-1");
        assert_eq!(chat_id, "cid-group");
    }

    #[test]
    fn test_streaming_support_detection() {
        let base = || {
            DingTalkChannel::new(
                "id".into(),
                "secret".into(),
                "dingtalk_test_alias",
                Arc::new(Vec::new),
            )
        };

        assert!(!base().supports_streaming());

        let ch_partial = base().with_streaming(StreamMode::Partial, 500);
        assert!(!ch_partial.supports_streaming()); // Still needs template_id

        let ch_with_template = base()
            .with_ai_card_template("card_template_123".into())
            .with_streaming(StreamMode::Partial, 500);
        assert!(ch_with_template.supports_streaming());

        let ch_multi = base().with_streaming(StreamMode::MultiMessage, 500);
        assert!(!ch_multi.supports_streaming());
    }

    #[tokio::test]
    async fn test_send_draft_returns_none_when_streaming_disabled() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        )
        .with_streaming(StreamMode::Partial, 500);

        let msg = SendMessage::new("test", "user1");
        let result = ch.send_draft(&msg).await.expect("send_draft ok");
        assert!(result.is_none()); // No template_id configured
    }

    #[tokio::test]
    async fn test_update_draft_is_noop_when_disabled() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        );

        // Should be no-op when streaming is disabled
        ch.update_draft("user1", "card-1", "hello")
            .await
            .expect("ok");
        ch.finalize_draft("user1", "card-1", "hello")
            .await
            .expect("ok");
        ch.cancel_draft("user1", "card-1").await.expect("ok");
    }

    #[tokio::test]
    async fn test_update_draft_ignores_empty_message_id() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        )
        .with_streaming(StreamMode::Partial, 500)
        .with_ai_card_template("tpl-001".into());

        // Should ignore empty message_id
        ch.update_draft("user1", "", "hello").await.expect("ok");
        ch.finalize_draft("user1", "", "hello").await.expect("ok");
        ch.cancel_draft("user1", "").await.expect("ok");
    }
}
