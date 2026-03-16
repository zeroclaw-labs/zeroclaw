use super::traits::{Channel, ChannelMessage, SendMessage};
use crate::config::StreamMode;
use aes::Aes256;
use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine as _;
use cbc::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};
use futures_util::{SinkExt, StreamExt};
use md5 as md5_crate;
use parking_lot::Mutex;
use rand::RngExt;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

// ── Constants ────────────────────────────────────────────────────────

const WECOM_WS_URL: &str = "wss://openws.work.weixin.qq.com";
const WECOM_BACKOFF_INITIAL_SECS: u64 = 5;
const WECOM_BACKOFF_MAX_SECS: u64 = 60;
const WECOM_PING_INTERVAL_SECS: u64 = 30;
const WECOM_SUBSCRIBE_TIMEOUT_SECS: u64 = 10;
const WECOM_COMMAND_TIMEOUT_SECS: u64 = 10;
const WECOM_HTTP_TIMEOUT_SECS: u64 = 60;
const WECOM_WS_READY_WAIT_SECS: u64 = 10;
const WECOM_WS_READY_POLL_MILLIS: u64 = 100;
const WECOM_STREAM_CONFLICT_MAX_RETRIES: usize = 3;
const WECOM_STREAM_CONFLICT_RETRY_BASE_MILLIS: u64 = 150;

const WECOM_MARKDOWN_MAX_BYTES: usize = 20_480;
const WECOM_MARKDOWN_CHUNK_BYTES: usize = 8_000;
const WECOM_EMOJIS: &[&str] = &[
    "\u{1F642}",
    "\u{1F604}",
    "\u{1F91D}",
    "\u{1F680}",
    "\u{1F44C}",
];
const WECOM_FILE_CLEANUP_INTERVAL_SECS: u64 = 1800;
const WECOM_STREAM_BOOTSTRAP_CONTENT: &str =
    "\u{6b63}\u{5728}\u{5904}\u{7406}\u{4e2d}\u{ff0c}\u{8bf7}\u{7a0d}\u{5019}\u{3002}";
const WECOM_STREAM_MAX_IMAGES: usize = 10;
const WECOM_IMAGE_MAX_BYTES: usize = 10 * 1024 * 1024;

// ── WebSocket outbound command ───────────────────────────────────────

enum WsOutbound {
    Frame(Value),
}

// ── Internal types ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ParsedInbound {
    msg_id: String,
    msg_type: String,
    chat_type: String,
    chat_id: Option<String>,
    sender_userid: String,
    aibot_id: String,
    raw_payload: Value,
}

#[derive(Debug, Clone)]
struct ScopeDecision {
    conversation_scope: String,
    shared_group_history: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessDecision {
    Allowed,
    AllowlistMissing,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachmentKind {
    Image,
    File,
}

impl AttachmentKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::File => "file",
        }
    }
}

#[derive(Debug)]
enum NormalizedMessage {
    Ready(String),
    VoiceMissingTranscript,
    Unsupported,
}

struct SimpleIdempotencyStore {
    seen: Mutex<HashSet<String>>,
}

impl SimpleIdempotencyStore {
    fn new() -> Self {
        Self {
            seen: Mutex::new(HashSet::new()),
        }
    }
    fn record_if_new(&self, key: &str) -> bool {
        self.seen.lock().insert(key.to_string())
    }
}

#[derive(Clone)]
struct WeComRuntimeConfig {
    workspace_dir: PathBuf,
    allowed_users: Vec<String>,
    allowed_groups: Vec<String>,
    file_retention_days: u32,
    max_file_size_bytes: u64,
    stream_mode: StreamMode,
}

#[derive(Debug, Clone)]
struct StreamImageItem {
    base64: String,
    md5: String,
}

// ── MediaDecryptor (per-attachment AES key) ──────────────────────────

struct MediaDecryptor;

impl MediaDecryptor {
    /// Decrypt WeCom media attachment using per-message AES key.
    /// AES-256-CBC, IV = first 16 bytes of key, WeCom-style PKCS padding.
    fn decrypt(aeskey_b64: &str, encrypted: &[u8]) -> Result<Vec<u8>> {
        let raw_key = base64::engine::general_purpose::STANDARD
            .decode(aeskey_b64.trim())
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(aeskey_b64.trim()))
            .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(aeskey_b64.trim()))
            .context("failed to decode WeCom media aeskey")?;

        if raw_key.len() < 32 {
            anyhow::bail!(
                "WeCom media aeskey too short: expected >= 32 bytes, got {}",
                raw_key.len()
            );
        }

        let key = &raw_key[..32];
        let iv = &key[..16];

        let mut buf = encrypted.to_vec();
        let plaintext = cbc::Decryptor::<Aes256>::new(key.into(), iv.into())
            .decrypt_padded_mut::<NoPadding>(&mut buf)
            .map_err(|_| anyhow::anyhow!("failed to decrypt WeCom media attachment"))?;
        Ok(strip_wecom_padding(plaintext)?.to_vec())
    }
}

// ── WeComWsChannel struct ────────────────────────────────────────────

/// WeCom (企业微信) channel — WebSocket long-connection mode.
///
/// Connects to `wss://openws.work.weixin.qq.com`, subscribes with bot_id + secret.
/// Inbound messages arrive as plaintext JSON frames (no encryption).
/// Outbound replies are pushed directly via WS frames (streaming supported).
/// Media attachments are encrypted per-URL with individual AES keys.
#[derive(Clone)]
pub struct WeComWsChannel {
    bot_id: String,
    secret: String,
    cfg: WeComRuntimeConfig,
    client: reqwest::Client,
    ws_tx: Arc<tokio::sync::Mutex<Option<mpsc::Sender<WsOutbound>>>>,
    pending_responses:
        Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<Result<()>>>>>,
    respond_msg_locks: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    last_cleanup: Arc<Mutex<Instant>>,
    idempotency: Arc<SimpleIdempotencyStore>,
    req_id_map: Arc<Mutex<HashMap<String, String>>>, // stream_id → req_id
}

// ── Construction + WS helpers ────────────────────────────────────────

impl WeComWsChannel {
    pub fn new(
        config: &crate::config::schema::WeComWsConfig,
        workspace_dir: &Path,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(WECOM_HTTP_TIMEOUT_SECS))
            .build()
            .context("failed to initialize WeCom HTTP client")?;

        Ok(Self {
            bot_id: config.bot_id.clone(),
            secret: config.secret.clone(),
            cfg: WeComRuntimeConfig {
                workspace_dir: workspace_dir.to_path_buf(),
                allowed_users: normalize_wecom_allowlist(config.allowed_users.clone()),
                allowed_groups: normalize_wecom_allowlist(config.allowed_groups.clone()),
                file_retention_days: config.file_retention_days,
                max_file_size_bytes: config.max_file_size_mb.saturating_mul(1024 * 1024),
                stream_mode: config.stream_mode,
            },
            client,
            ws_tx: Arc::new(tokio::sync::Mutex::new(None)),
            pending_responses: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            respond_msg_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            last_cleanup: Arc::new(Mutex::new(Instant::now())),
            idempotency: Arc::new(SimpleIdempotencyStore::new()),
            req_id_map: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    async fn wait_for_ws_sender(&self) -> Result<mpsc::Sender<WsOutbound>> {
        let deadline = Instant::now() + Duration::from_secs(WECOM_WS_READY_WAIT_SECS);

        loop {
            if let Some(tx) = self.ws_tx.lock().await.as_ref().cloned() {
                return Ok(tx);
            }

            if Instant::now() >= deadline {
                anyhow::bail!("WeCom WebSocket not connected");
            }

            tokio::time::sleep(Duration::from_millis(WECOM_WS_READY_POLL_MILLIS)).await;
        }
    }

    /// Send a JSON frame through the WebSocket outbound channel.
    async fn ws_send_frame(&self, frame: Value) -> Result<()> {
        let tx = self.wait_for_ws_sender().await?;
        tx.send(WsOutbound::Frame(frame))
            .await
            .map_err(|_| anyhow::anyhow!("WeCom WS outbound channel closed"))
    }

    async fn ws_send_frame_and_wait_for_response(
        &self,
        frame: Value,
        req_id: &str,
        command: &str,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending_responses
            .lock()
            .await
            .insert(req_id.to_string(), tx);

        if let Err(err) = self.ws_send_frame(frame).await {
            self.pending_responses.lock().await.remove(req_id);
            return Err(err);
        }

        match tokio::time::timeout(Duration::from_secs(WECOM_COMMAND_TIMEOUT_SECS), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => anyhow::bail!(
                "WeCom WS {command} response channel closed before ack (req_id={req_id})"
            ),
            Err(_) => {
                self.pending_responses.lock().await.remove(req_id);
                anyhow::bail!(
                    "WeCom WS {command} ack timeout after {}s (req_id={req_id})",
                    WECOM_COMMAND_TIMEOUT_SECS
                );
            }
        }
    }

    async fn maybe_handle_command_response(&self, frame: &Value) -> bool {
        let Some(req_id) = frame
            .get("headers")
            .and_then(|headers| headers.get("req_id"))
            .and_then(Value::as_str)
        else {
            return false;
        };

        let Some(errcode) = frame.get("errcode").and_then(Value::as_i64) else {
            return false;
        };

        let errmsg = frame
            .get("errmsg")
            .and_then(Value::as_str)
            .unwrap_or("unknown");

        if let Some(waiter) = self.pending_responses.lock().await.remove(req_id) {
            let result = if errcode == 0 {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "WeCom command failed: req_id={req_id} errcode={errcode} errmsg={errmsg}"
                ))
            };
            let _ = waiter.send(result);
            return true;
        }

        if errcode == 0 {
            tracing::debug!(
                req_id,
                errcode,
                errmsg,
                "[wecom_ws] unsolicited command response"
            );
        } else {
            tracing::warn!(
                req_id,
                errcode,
                errmsg,
                "[wecom_ws] command response failed without a waiter"
            );
        }

        true
    }

    async fn respond_msg_lock_for_req_id(&self, req_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.respond_msg_locks
            .lock()
            .await
            .entry(req_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    async fn cleanup_respond_msg_lock(&self, req_id: &str) {
        self.respond_msg_locks.lock().await.remove(req_id);
    }

    async fn fail_pending_responses(&self, reason: &str) {
        let pending = {
            let mut guard = self.pending_responses.lock().await;
            std::mem::take(&mut *guard)
        };

        for (req_id, waiter) in pending {
            let _ = waiter.send(Err(anyhow::anyhow!(
                "WeCom WebSocket disconnected before response: req_id={req_id} reason={reason}"
            )));
        }
    }

    fn access_decision(&self, inbound: &ParsedInbound) -> AccessDecision {
        evaluate_access_decision(&self.cfg.allowed_users, &self.cfg.allowed_groups, inbound)
    }

    async fn respond_access_denied(
        &self,
        req_id: &str,
        inbound: &ParsedInbound,
        decision: AccessDecision,
    ) {
        let message = build_access_denied_message(inbound, decision);
        let stream_id = next_stream_id();
        if let Err(err) = self
            .ws_queue_respond_msg(req_id, &stream_id, &message, true, &[])
            .await
        {
            tracing::warn!(
                sender_userid = %inbound.sender_userid,
                chat_type = %inbound.chat_type,
                chat_id = %inbound.chat_id.as_deref().unwrap_or("-"),
                error = %format_args!("{err:#}"),
                "[wecom_ws] failed to send access-denied response"
            );
        }
    }

    /// Send an `aibot_respond_msg` streaming frame.
    fn build_respond_msg_frame(
        req_id: &str,
        stream_id: &str,
        content: &str,
        finish: bool,
        images: &[StreamImageItem],
    ) -> Value {
        let stream_obj = serde_json::json!({
            "id": stream_id,
            "finish": finish,
            "content": normalize_stream_content(content),
        });
        if !images.is_empty() {
            let dropped_image_payload_chars: usize = images
                .iter()
                .map(|img| img.base64.len() + img.md5.len())
                .sum();
            tracing::warn!(
                image_count = images.len(),
                dropped_image_payload_chars,
                "WeCom WS stream replies do not currently support msg_item images; dropping images"
            );
        }
        serde_json::json!({
            "cmd": "aibot_respond_msg",
            "headers": { "req_id": req_id },
            "body": {
                "msgtype": "stream",
                "stream": stream_obj,
            },
        })
    }

    async fn ws_queue_respond_msg(
        &self,
        req_id: &str,
        stream_id: &str,
        content: &str,
        finish: bool,
        images: &[StreamImageItem],
    ) -> Result<()> {
        let frame = Self::build_respond_msg_frame(req_id, stream_id, content, finish, images);
        self.ws_send_frame(frame).await
    }

    async fn ws_send_respond_msg(
        &self,
        req_id: &str,
        stream_id: &str,
        content: &str,
        finish: bool,
        images: &[StreamImageItem],
    ) -> Result<()> {
        let frame = Self::build_respond_msg_frame(req_id, stream_id, content, finish, images);
        if req_id.is_empty() {
            return self.ws_send_frame(frame).await;
        }

        let stream_lock = self.respond_msg_lock_for_req_id(req_id).await;
        let _guard = stream_lock.lock().await;
        let mut attempt = 0usize;

        let result = loop {
            match self
                .ws_send_frame_and_wait_for_response(frame.clone(), req_id, "aibot_respond_msg")
                .await
            {
                Ok(()) => break Ok(()),
                Err(err)
                    if is_wecom_data_version_conflict_error(&err)
                        && attempt < WECOM_STREAM_CONFLICT_MAX_RETRIES =>
                {
                    let retry_in_ms =
                        WECOM_STREAM_CONFLICT_RETRY_BASE_MILLIS.saturating_mul(1u64 << attempt);
                    attempt += 1;
                    tracing::warn!(
                        req_id,
                        stream_id,
                        attempt,
                        retry_in_ms,
                        "WeCom stream reply hit data-version conflict; retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(retry_in_ms)).await;
                }
                Err(err) => break Err(err),
            }
        };

        if finish {
            self.cleanup_respond_msg_lock(req_id).await;
        }

        result
    }

    // ── file cleanup ─────────────────────────────────────────────────

    fn maybe_cleanup_files(&self) {
        let now = Instant::now();
        {
            let mut last = self.last_cleanup.lock();
            if now.duration_since(*last) < Duration::from_secs(WECOM_FILE_CLEANUP_INTERVAL_SECS) {
                return;
            }
            *last = now;
        }

        let retention = Duration::from_secs(u64::from(self.cfg.file_retention_days) * 86_400);
        let root = self.cfg.workspace_dir.join("wecom_ws_files");
        tokio::spawn(async move {
            cleanup_inbox_files(root, retention).await;
        });
    }

    // ── WS message dispatch ──────────────────────────────────────────

    /// Returns `true` if the caller should trigger reconnection.
    async fn handle_ws_message(&self, frame: Value, tx: &mpsc::Sender<ChannelMessage>) -> bool {
        if self.maybe_handle_command_response(&frame).await {
            return false;
        }

        let cmd = frame.get("cmd").and_then(Value::as_str).unwrap_or("");

        match cmd {
            "aibot_msg_callback" => {
                let channel = self.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    channel.handle_msg_callback(frame, &tx).await;
                });
                false
            }
            "aibot_event_callback" => self.handle_event_callback(frame).await,
            _ => {
                tracing::debug!("[wecom_ws] ignoring WS frame cmd={cmd}");
                false
            }
        }
    }

    // ── Message callback handling ────────────────────────────────────

    async fn handle_msg_callback(&self, frame: Value, tx: &mpsc::Sender<ChannelMessage>) {
        let req_id = frame
            .get("headers")
            .and_then(|h| h.get("req_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let body = match frame.get("body") {
            Some(b) => b.clone(),
            None => {
                tracing::warn!("[wecom_ws] msg_callback missing body");
                return;
            }
        };

        let parsed = match parse_inbound_payload(body) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!("[wecom_ws] msg_callback parse failed: {err:#}");
                return;
            }
        };

        // Idempotency check
        if !parsed.msg_id.is_empty() {
            let key = format!("wecom_ws_msg_{}", parsed.msg_id);
            if !self.idempotency.record_if_new(&key) {
                return;
            }
        }

        let scopes = compute_scopes(&parsed);

        // Log inbound info
        let preview = crate::util::truncate_with_ellipsis(&inbound_content_preview(&parsed), 80);
        let msg_id_str = if parsed.msg_id.trim().is_empty() {
            "-"
        } else {
            parsed.msg_id.as_str()
        };
        tracing::info!(
            "[wecom_ws] from {} in {}: {} (msg_type={}, msg_id={})",
            parsed.sender_userid,
            scopes.conversation_scope,
            preview,
            parsed.msg_type,
            msg_id_str
        );

        match self.access_decision(&parsed) {
            AccessDecision::Allowed => {}
            AccessDecision::AllowlistMissing => {
                tracing::warn!(
                    sender_userid = %parsed.sender_userid,
                    chat_type = %parsed.chat_type,
                    chat_id = %parsed.chat_id.as_deref().unwrap_or("-"),
                    "[wecom_ws] inbound denied because allowlist is not configured"
                );
                self.respond_access_denied(&req_id, &parsed, AccessDecision::AllowlistMissing)
                    .await;
                return;
            }
            AccessDecision::Denied => {
                tracing::warn!(
                    sender_userid = %parsed.sender_userid,
                    chat_type = %parsed.chat_type,
                    chat_id = %parsed.chat_id.as_deref().unwrap_or("-"),
                    "[wecom_ws] inbound denied by allowlist"
                );
                self.respond_access_denied(&req_id, &parsed, AccessDecision::Denied)
                    .await;
                return;
            }
        }

        self.maybe_cleanup_files();

        // ── Command detection ────────────────────────────────────────

        let stop_text = extract_stop_signal_text(&parsed).unwrap_or_default();

        // Clear session
        if is_clear_session_command(&stop_text) {
            tracing::info!(
                "WeCom session cleared: scope={} msg_id={}",
                scopes.conversation_scope,
                parsed.msg_id
            );
            let _ = tx
                .send(ChannelMessage {
                    id: parsed.msg_id.clone(),
                    sender: parsed.sender_userid.clone(),
                    reply_target: scopes.conversation_scope.clone(),
                    content: "/new".to_string(),
                    channel: "wecom_ws".to_string(),
                    timestamp: bytes_timestamp_now(),
                    thread_ts: Some(req_id),
                })
                .await;
            return;
        }

        // Stop command
        if contains_stop_command(&stop_text) {
            let msg =
                "\u{5df2}\u{505c}\u{6b62}\u{5f53}\u{524d}\u{6d88}\u{606f}\u{5904}\u{7406}\u{3002}";
            let stream_id = next_stream_id();
            let _ = self
                .ws_queue_respond_msg(&req_id, &stream_id, msg, true, &[])
                .await;
            let _ = tx
                .send(ChannelMessage {
                    id: parsed.msg_id.clone(),
                    sender: parsed.sender_userid.clone(),
                    reply_target: scopes.conversation_scope.clone(),
                    content: "/new".to_string(),
                    channel: "wecom_ws".to_string(),
                    timestamp: bytes_timestamp_now(),
                    thread_ts: None,
                })
                .await;
            return;
        }

        if let Some(runtime_command) = extract_runtime_model_switch_command(&stop_text) {
            tracing::info!(
                "WeCom runtime command forwarded: scope={} msg_id={} command={}",
                scopes.conversation_scope,
                parsed.msg_id,
                runtime_command
            );
            let _ = tx
                .send(ChannelMessage {
                    id: parsed.msg_id.clone(),
                    sender: parsed.sender_userid.clone(),
                    reply_target: scopes.conversation_scope.clone(),
                    content: runtime_command,
                    channel: "wecom_ws".to_string(),
                    timestamp: bytes_timestamp_now(),
                    thread_ts: Some(req_id),
                })
                .await;
            return;
        }

        // Voice without transcript
        if is_voice_without_transcript(&parsed) {
            let msg = format!(
                "\u{6211}\u{73b0}\u{5728}\u{65e0}\u{6cd5}\u{5904}\u{7406}\u{8bed}\u{97f3}\u{6d88}\u{606f} {}",
                random_emoji()
            );
            let stream_id = next_stream_id();
            let _ = self
                .ws_queue_respond_msg(&req_id, &stream_id, &msg, true, &[])
                .await;
            return;
        }

        // Unsupported message type
        if !is_model_supported_msgtype(&parsed.msg_type) {
            tracing::info!(
                "WeCom unsupported message ignored: msg_type={} msg_id={}",
                parsed.msg_type,
                parsed.msg_id
            );
            return;
        }

        // ── Forward normal message to framework ──────────────────────

        let channel_self = self.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut inbound = parsed;
            channel_self
                .materialize_quote_attachments(&mut inbound)
                .await;
            let normalized = channel_self.normalize_message(&inbound).await;

            let content = match normalized {
                NormalizedMessage::VoiceMissingTranscript => {
                    let msg = format!(
                        "\u{6211}\u{73b0}\u{5728}\u{65e0}\u{6cd5}\u{5904}\u{7406}\u{8bed}\u{97f3}\u{6d88}\u{606f} {}",
                        random_emoji()
                    );
                    let stream_id = next_stream_id();
                    let _ = channel_self
                        .ws_queue_respond_msg(&req_id, &stream_id, &msg, true, &[])
                        .await;
                    return;
                }
                NormalizedMessage::Unsupported => {
                    let msg = "\u{6682}\u{4e0d}\u{652f}\u{6301}\u{8be5}\u{6d88}\u{606f}\u{7c7b}\u{578b}\u{3002}";
                    let stream_id = next_stream_id();
                    let _ = channel_self
                        .ws_queue_respond_msg(&req_id, &stream_id, msg, true, &[])
                        .await;
                    return;
                }
                NormalizedMessage::Ready(content) => content,
            };

            let composed = compose_content_for_framework(&inbound, &content);

            tracing::info!(
                "WeCom: forwarding to framework: msg_id={} req_id={} scope={}",
                inbound.msg_id,
                req_id,
                scopes.conversation_scope
            );

            let _ = tx
                .send(ChannelMessage {
                    id: inbound.msg_id.clone(),
                    sender: inbound.sender_userid.clone(),
                    reply_target: scopes.conversation_scope.clone(),
                    content: composed,
                    channel: "wecom_ws".to_string(),
                    timestamp: bytes_timestamp_now(),
                    thread_ts: Some(req_id),
                })
                .await;
        });
    }

    // ── Event callback handling ──────────────────────────────────────

    /// Returns `true` if the caller should trigger reconnection.
    async fn handle_event_callback(&self, frame: Value) -> bool {
        let req_id = frame
            .get("headers")
            .and_then(|h| h.get("req_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let body = frame.get("body").cloned().unwrap_or(Value::Null);
        let event_type = parse_event_type(&body).unwrap_or_else(|| "unknown".to_string());

        match event_type.as_str() {
            "enter_chat" => {
                let content = format!(
                    "\u{4f60}\u{597d}\u{ff0c}\u{6b22}\u{8fce}\u{6765}\u{627e}\u{6211}\u{804a}\u{5929} {}",
                    random_emoji()
                );
                let welcome = serde_json::json!({
                    "cmd": "aibot_respond_welcome_msg",
                    "headers": { "req_id": req_id },
                    "body": {
                        "msgtype": "text",
                        "text": { "content": content }
                    }
                });
                let _ = self.ws_send_frame(welcome).await;
                false
            }
            "template_card_event" => {
                let event_key =
                    extract_template_card_event_key(&body).unwrap_or_else(|| "-".to_string());
                tracing::info!("WeCom template_card_event received: event_key={event_key}");
                false
            }
            "feedback_event" => {
                let summary = extract_feedback_event_summary(&body)
                    .unwrap_or_else(|| "feedback=invalid-payload".to_string());
                tracing::info!("WeCom feedback_event received: {summary}");
                false
            }
            "disconnected_event" => {
                tracing::warn!("[wecom_ws] received disconnected_event, triggering reconnect");
                true
            }
            other => {
                tracing::debug!("[wecom_ws] ignoring event_type={other}");
                false
            }
        }
    }

    // ── Attachment handling ──────────────────────────────────────────

    async fn materialize_quote_attachments(&self, inbound: &mut ParsedInbound) {
        let quote_type = inbound
            .raw_payload
            .get("quote")
            .and_then(|v| v.get("msgtype"))
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");

        if quote_type == "image" {
            let quote_obj = inbound
                .raw_payload
                .get("quote")
                .and_then(|v| v.get("image"));
            let quote_url = quote_obj
                .and_then(|v| v.get("url"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToOwned::to_owned);
            let aeskey = quote_obj
                .and_then(|v| v.get("aeskey"))
                .and_then(Value::as_str);
            if let Some(url) = quote_url {
                let marker = match self
                    .download_and_store_attachment(&url, AttachmentKind::Image, inbound, aeskey)
                    .await
                {
                    Ok(value) => value,
                    Err(err) => {
                        log_attachment_processing_failure(
                            "WeCom quote image processing failed",
                            &err,
                            inbound,
                            AttachmentKind::Image,
                            &url,
                        );
                        "[\u{5f15}\u{7528}\u{56fe}\u{7247}\u{4e0b}\u{8f7d}\u{5931}\u{8d25}]"
                            .to_string()
                    }
                };
                if let Some(quote) = inbound.raw_payload.get_mut("quote") {
                    quote["image"] = serde_json::json!({ "local_path": marker });
                }
            }
            return;
        }

        if quote_type == "file" {
            let quote_obj = inbound.raw_payload.get("quote").and_then(|v| v.get("file"));
            let quote_url = quote_obj
                .and_then(|v| v.get("url"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToOwned::to_owned);
            let aeskey = quote_obj
                .and_then(|v| v.get("aeskey"))
                .and_then(Value::as_str);
            if let Some(url) = quote_url {
                let marker = match self
                    .download_and_store_attachment(&url, AttachmentKind::File, inbound, aeskey)
                    .await
                {
                    Ok(value) => value,
                    Err(err) => {
                        log_attachment_processing_failure(
                            "WeCom quote file processing failed",
                            &err,
                            inbound,
                            AttachmentKind::File,
                            &url,
                        );
                        "[\u{5f15}\u{7528}\u{6587}\u{4ef6}\u{4e0b}\u{8f7d}\u{5931}\u{8d25}]"
                            .to_string()
                    }
                };
                if let Some(quote) = inbound.raw_payload.get_mut("quote") {
                    quote["file"] = serde_json::json!({ "local_path": marker });
                }
            }
            return;
        }

        if quote_type == "mixed" {
            let quote_images: Vec<(usize, String, Option<String>)> = inbound
                .raw_payload
                .get("quote")
                .and_then(|v| v.get("mixed"))
                .and_then(|v| v.get("msg_item"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .enumerate()
                        .filter_map(|(idx, item)| {
                            let item_type = item
                                .get("msgtype")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            if item_type != "image" {
                                return None;
                            }
                            let img = item.get("image")?;
                            let url = img
                                .get("url")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|v| !v.is_empty())?;
                            let aeskey = img
                                .get("aeskey")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned);
                            Some((idx, url.to_string(), aeskey))
                        })
                        .collect()
                })
                .unwrap_or_default();

            if quote_images.is_empty() {
                return;
            }

            let mut results: Vec<(usize, String)> = Vec::with_capacity(quote_images.len());
            for (idx, url, aeskey) in &quote_images {
                let marker = match self
                    .download_and_store_attachment(
                        url,
                        AttachmentKind::Image,
                        inbound,
                        aeskey.as_deref(),
                    )
                    .await
                {
                    Ok(value) => value,
                    Err(err) => {
                        log_attachment_processing_failure(
                            "WeCom quote mixed image processing failed",
                            &err,
                            inbound,
                            AttachmentKind::Image,
                            url,
                        );
                        "[\u{5f15}\u{7528}\u{56fe}\u{7247}\u{4e0b}\u{8f7d}\u{5931}\u{8d25}]"
                            .to_string()
                    }
                };
                results.push((*idx, marker));
            }

            if let Some(items) = inbound
                .raw_payload
                .get_mut("quote")
                .and_then(|v| v.get_mut("mixed"))
                .and_then(|v| v.get_mut("msg_item"))
                .and_then(Value::as_array_mut)
            {
                for (idx, marker) in results {
                    if let Some(item) = items.get_mut(idx) {
                        item["image"] = serde_json::json!({ "local_path": marker });
                    }
                }
            }
        }
    }

    async fn normalize_message(&self, inbound: &ParsedInbound) -> NormalizedMessage {
        match inbound.msg_type.as_str() {
            "text" => {
                let content = inbound
                    .raw_payload
                    .get("text")
                    .and_then(|v| v.get("content"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();

                if content.is_empty() {
                    NormalizedMessage::Unsupported
                } else {
                    NormalizedMessage::Ready(content)
                }
            }
            "voice" => {
                let content = inbound
                    .raw_payload
                    .get("voice")
                    .and_then(|v| v.get("content"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();

                if content.is_empty() {
                    NormalizedMessage::VoiceMissingTranscript
                } else {
                    NormalizedMessage::Ready(format!("[Voice transcript]\n{content}"))
                }
            }
            "image" => {
                let image_obj = inbound.raw_payload.get("image");
                let url = image_obj
                    .and_then(|v| v.get("url"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                let aeskey = image_obj
                    .and_then(|v| v.get("aeskey"))
                    .and_then(Value::as_str);

                if url.is_empty() {
                    return NormalizedMessage::Unsupported;
                }

                match self
                    .download_and_store_attachment(url, AttachmentKind::Image, inbound, aeskey)
                    .await
                {
                    Ok(marker) => NormalizedMessage::Ready(marker),
                    Err(err) => {
                        log_attachment_processing_failure(
                            "WeCom image processing failed",
                            &err,
                            inbound,
                            AttachmentKind::Image,
                            url,
                        );
                        NormalizedMessage::Ready(
                            "[Image attachment processing failed; please continue without this image.]"
                                .to_string(),
                        )
                    }
                }
            }
            "file" => {
                let file_obj = inbound.raw_payload.get("file");
                let url = file_obj
                    .and_then(|v| v.get("url"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                let aeskey = file_obj
                    .and_then(|v| v.get("aeskey"))
                    .and_then(Value::as_str);

                if url.is_empty() {
                    return NormalizedMessage::Unsupported;
                }

                match self
                    .download_and_store_attachment(url, AttachmentKind::File, inbound, aeskey)
                    .await
                {
                    Ok(marker) => NormalizedMessage::Ready(marker),
                    Err(err) => {
                        log_attachment_processing_failure(
                            "WeCom file processing failed",
                            &err,
                            inbound,
                            AttachmentKind::File,
                            url,
                        );
                        NormalizedMessage::Ready(
                            "[File attachment processing failed; please continue without this file.]"
                                .to_string(),
                        )
                    }
                }
            }
            "mixed" => {
                let mut text_parts = Vec::new();
                if let Some(items) = inbound
                    .raw_payload
                    .get("mixed")
                    .and_then(|v| v.get("msg_item"))
                    .and_then(Value::as_array)
                {
                    for item in items {
                        let item_type = item
                            .get("msgtype")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if item_type == "text" {
                            if let Some(text) = item
                                .get("text")
                                .and_then(|v| v.get("content"))
                                .and_then(Value::as_str)
                            {
                                let trimmed = text.trim();
                                if !trimmed.is_empty() {
                                    text_parts.push(trimmed.to_string());
                                }
                            }
                        } else if item_type == "image" {
                            let img = item.get("image");
                            let url = img.and_then(|v| v.get("url")).and_then(Value::as_str);
                            let aeskey = img.and_then(|v| v.get("aeskey")).and_then(Value::as_str);
                            if let Some(url) = url {
                                match self
                                    .download_and_store_attachment(
                                        url,
                                        AttachmentKind::Image,
                                        inbound,
                                        aeskey,
                                    )
                                    .await
                                {
                                    Ok(marker) => text_parts.push(marker),
                                    Err(err) => {
                                        log_attachment_processing_failure(
                                            "WeCom mixed image processing failed",
                                            &err,
                                            inbound,
                                            AttachmentKind::Image,
                                            url,
                                        );
                                        text_parts.push(
                                            "[Image attachment processing failed in mixed message.]"
                                                .to_string(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                if text_parts.is_empty() {
                    NormalizedMessage::Unsupported
                } else {
                    NormalizedMessage::Ready(text_parts.join("\n\n"))
                }
            }
            other => {
                tracing::info!(
                    "[wecom_ws] unsupported msg_type={other}, raw_payload={}",
                    inbound.raw_payload
                );
                NormalizedMessage::Unsupported
            }
        }
    }

    async fn download_and_store_attachment(
        &self,
        url: &str,
        kind: AttachmentKind,
        inbound: &ParsedInbound,
        aeskey: Option<&str>,
    ) -> Result<String> {
        if self.cfg.max_file_size_bytes == 0 {
            anyhow::bail!("WeCom max_file_size_bytes is zero");
        }

        let started = Instant::now();
        let chat_id = inbound.chat_id.as_deref().unwrap_or("single");
        let url_target = summarize_attachment_url_for_log(url);
        tracing::info!(
            msg_id = %inbound.msg_id,
            msg_type = %inbound.msg_type,
            chat_type = %inbound.chat_type,
            chat_id = %chat_id,
            sender_userid = %inbound.sender_userid,
            attachment_kind = %kind.as_str(),
            url_target = %url_target,
            has_aeskey = aeskey.is_some(),
            timeout_secs = WECOM_HTTP_TIMEOUT_SECS,
            "WeCom attachment download started"
        );

        let response = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| {
                format!(
                    "failed to download WeCom attachment: kind={} msg_id={} url_target={} elapsed_ms={}",
                    kind.as_str(),
                    inbound.msg_id,
                    url_target,
                    started.elapsed().as_millis(),
                )
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let body_preview = truncate_for_log(&body, 512);
            anyhow::bail!(
                "WeCom attachment download failed: kind={} msg_id={} url_target={} status={} body_preview={}",
                kind.as_str(),
                inbound.msg_id,
                url_target,
                status,
                body_preview
            );
        }

        if let Some(len) = response.content_length() {
            if len > self.cfg.max_file_size_bytes {
                tracing::warn!(
                    msg_id = %inbound.msg_id,
                    attachment_kind = %kind.as_str(),
                    declared_bytes = len,
                    max_file_size_bytes = self.cfg.max_file_size_bytes,
                    "WeCom attachment skipped: declared size exceeds configured limit"
                );
                return Ok(format!(
                    "[AttachmentTooLarge kind={:?} size={}B limit={}B]",
                    kind, len, self.cfg.max_file_size_bytes
                ));
            }
        }

        let bytes = response
            .bytes()
            .await
            .with_context(|| {
                format!(
                    "failed to read WeCom attachment bytes: kind={} msg_id={} url_target={} elapsed_ms={}",
                    kind.as_str(),
                    inbound.msg_id,
                    url_target,
                    started.elapsed().as_millis(),
                )
            })?;

        if bytes.len() as u64 > self.cfg.max_file_size_bytes {
            tracing::warn!(
                msg_id = %inbound.msg_id,
                attachment_kind = %kind.as_str(),
                actual_bytes = bytes.len(),
                max_file_size_bytes = self.cfg.max_file_size_bytes,
                "WeCom attachment skipped: payload exceeds configured limit"
            );
            return Ok(format!(
                "[AttachmentTooLarge kind={:?} size={}B limit={}B]",
                kind,
                bytes.len(),
                self.cfg.max_file_size_bytes
            ));
        }

        // Decrypt if aeskey is present; otherwise use raw bytes
        let decrypted = match aeskey {
            Some(key) => MediaDecryptor::decrypt(key, &bytes).with_context(|| {
                format!(
                    "failed to decrypt WeCom attachment: kind={} msg_id={} url_target={} encrypted_bytes={}",
                    kind.as_str(),
                    inbound.msg_id,
                    url_target,
                    bytes.len(),
                )
            })?,
            None => bytes.to_vec(),
        };
        let decrypted_len = decrypted.len();

        let ext = match kind {
            AttachmentKind::Image => "png",
            AttachmentKind::File => "bin",
        };
        let safe_scope = normalize_scope_component(&format!(
            "{}_{}",
            inbound.chat_id.as_deref().unwrap_or("single"),
            inbound.sender_userid
        ));
        let ts = bytes_timestamp_now();
        let file_name = format!(
            "{safe_scope}_{ts}_{}_{}.{}",
            inbound.msg_id,
            random_ascii_token(6),
            ext
        );

        let dir = self.cfg.workspace_dir.join("wecom_ws_files");
        tokio::fs::create_dir_all(&dir).await.with_context(|| {
            format!(
                "failed to create WeCom inbox directory: msg_id={} path={}",
                inbound.msg_id,
                dir.display()
            )
        })?;
        let path = dir.join(file_name);

        tokio::fs::write(&path, decrypted).await.with_context(|| {
            format!(
                "failed to persist WeCom attachment: kind={} msg_id={} path={}",
                kind.as_str(),
                inbound.msg_id,
                path.display()
            )
        })?;

        self.maybe_cleanup_files();

        let abs = path.canonicalize().unwrap_or(path);
        tracing::info!(
            msg_id = %inbound.msg_id,
            attachment_kind = %kind.as_str(),
            url_target = %url_target,
            encrypted_bytes = bytes.len(),
            decrypted_bytes = decrypted_len,
            local_path = %abs.display(),
            elapsed_ms = started.elapsed().as_millis(),
            "WeCom attachment download completed"
        );
        match kind {
            AttachmentKind::Image => Ok(format!("[IMAGE:{}]", abs.display())),
            AttachmentKind::File => Ok(format!("[Document: {}]", abs.display())),
        }
    }

    async fn send_markdown_chunks_to_scope(&self, scope: &str, content: &str) -> Result<()> {
        let (chat_type, chatid) = parse_scope(scope)?;
        let chunks = split_markdown_chunks(content);

        tracing::info!(
            "WeCom: sending message to scope={}, len={}, chunks={}",
            scope,
            content.len(),
            chunks.len()
        );

        let total_chunks = chunks.len();
        for (idx, chunk) in chunks.into_iter().enumerate() {
            let req_id = random_ascii_token(16);
            let chunk_len = chunk.len();
            let frame = serde_json::json!({
                "cmd": "aibot_send_msg",
                "headers": { "req_id": req_id },
                "body": {
                    "chatid": chatid,
                    "chat_type": chat_type,
                    "msgtype": "markdown",
                    "markdown": { "content": chunk }
                }
            });
            self.ws_send_frame_and_wait_for_response(frame, &req_id, "aibot_send_msg")
                .await?;
            tracing::info!(
                scope = %scope,
                req_id = %req_id,
                chunk_index = idx + 1,
                chunk_count = total_chunks,
                chunk_len,
                "WeCom send ack received"
            );
        }

        Ok(())
    }
}

// ── Channel trait impl ───────────────────────────────────────────────

#[async_trait]
impl Channel for WeComWsChannel {
    fn name(&self) -> &str {
        "wecom_ws"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        if let Some(req_id) = message
            .thread_ts
            .as_deref()
            .filter(|req_id| !req_id.is_empty())
        {
            let stream_id = next_stream_id();
            let (text_without_images, image_paths) = parse_image_markers(&message.content);
            let images = prepare_stream_images(&image_paths).await;
            let (stream_content, overflow) =
                split_stream_content_and_overflow(&text_without_images);

            self.ws_send_respond_msg(req_id, &stream_id, &stream_content, true, &images)
                .await?;

            if let Some(extra) = overflow {
                let extra_msg = format!("[补充消息]\n{extra}");
                self.send_markdown_chunks_to_scope(&message.recipient, &extra_msg)
                    .await?;
            }

            return Ok(());
        }

        self.send_markdown_chunks_to_scope(&message.recipient, &message.content)
            .await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        tracing::info!(
            "[wecom_ws] starting WebSocket listener (bot_id={})",
            self.bot_id
        );

        let mut backoff = WECOM_BACKOFF_INITIAL_SECS;

        loop {
            tracing::info!("[wecom_ws] connecting to {WECOM_WS_URL}");

            let ws_stream = match tokio_tungstenite::connect_async(WECOM_WS_URL).await {
                Ok((stream, _)) => {
                    tracing::info!("[wecom_ws] WebSocket connected");
                    stream
                }
                Err(err) => {
                    tracing::warn!(
                        "[wecom_ws] WebSocket connect failed: {err:#}, retrying in {backoff}s"
                    );
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(WECOM_BACKOFF_MAX_SECS);
                    continue;
                }
            };

            let (mut ws_write, mut ws_read) = ws_stream.split();

            // Send subscribe
            let subscribe_req_id = random_ascii_token(16);
            let subscribe = serde_json::json!({
                "cmd": "aibot_subscribe",
                "headers": { "req_id": subscribe_req_id },
                "body": {
                    "bot_id": self.bot_id,
                    "secret": self.secret,
                },
            });
            if let Err(err) = ws_write
                .send(WsMessage::Text(subscribe.to_string().into()))
                .await
            {
                tracing::warn!("[wecom_ws] subscribe send failed: {err:#}, retrying in {backoff}s");
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(WECOM_BACKOFF_MAX_SECS);
                continue;
            }

            // Wait for subscribe response
            let subscribe_ok = match tokio::time::timeout(
                Duration::from_secs(WECOM_SUBSCRIBE_TIMEOUT_SECS),
                ws_read.next(),
            )
            .await
            {
                Ok(Some(Ok(WsMessage::Text(text)))) => match serde_json::from_str::<Value>(&text) {
                    Ok(val) => {
                        if let Some(resp_req_id) = val
                            .get("headers")
                            .and_then(|h| h.get("req_id"))
                            .and_then(Value::as_str)
                        {
                            if resp_req_id != subscribe_req_id {
                                tracing::warn!(
                                    expected_req_id = %subscribe_req_id,
                                    got_req_id = %resp_req_id,
                                    "[wecom_ws] subscribe response req_id mismatch"
                                );
                            }
                        }
                        let errcode = val.get("errcode").and_then(Value::as_i64).unwrap_or(-1);
                        if errcode == 0 {
                            tracing::info!("[wecom_ws] subscribe succeeded");
                            true
                        } else {
                            let errmsg = val
                                .get("errmsg")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown");
                            tracing::error!(
                                "[wecom_ws] subscribe rejected: errcode={errcode} errmsg={errmsg}"
                            );
                            false
                        }
                    }
                    Err(err) => {
                        tracing::warn!("[wecom_ws] subscribe response parse failed: {err:#}");
                        false
                    }
                },
                Ok(Some(Ok(_))) => {
                    tracing::warn!("[wecom_ws] unexpected subscribe response frame type");
                    false
                }
                Ok(Some(Err(err))) => {
                    tracing::warn!("[wecom_ws] subscribe response read error: {err:#}");
                    false
                }
                Ok(None) => {
                    tracing::warn!("[wecom_ws] WebSocket closed before subscribe response");
                    false
                }
                Err(_) => {
                    tracing::warn!("[wecom_ws] subscribe response timeout");
                    false
                }
            };

            if !subscribe_ok {
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(WECOM_BACKOFF_MAX_SECS);
                continue;
            }

            // Create mpsc channel for outbound frames
            let (out_tx, mut out_rx) = mpsc::channel::<WsOutbound>(64);
            *self.ws_tx.lock().await = Some(out_tx);
            backoff = WECOM_BACKOFF_INITIAL_SECS; // reset on successful connect

            let mut ping_interval =
                tokio::time::interval(Duration::from_secs(WECOM_PING_INTERVAL_SECS));
            ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let mut should_reconnect = false;

            // Inner loop: process WS frames
            loop {
                tokio::select! {
                    _ = ping_interval.tick() => {
                        let ping = serde_json::json!({
                            "cmd": "ping",
                            "headers": { "req_id": random_ascii_token(16) },
                        });
                        if let Err(err) = ws_write
                            .send(WsMessage::Text(ping.to_string().into()))
                            .await
                        {
                            tracing::warn!("[wecom_ws] ping send failed: {err:#}");
                            break;
                        }
                    }
                    Some(outbound) = out_rx.recv() => {
                        match outbound {
                            WsOutbound::Frame(value) => {
                                if let Err(err) = ws_write
                                    .send(WsMessage::Text(value.to_string().into()))
                                    .await
                                {
                                    tracing::warn!(
                                        "[wecom_ws] outbound frame send failed: {err:#}"
                                    );
                                    break;
                                }
                            }
                        }
                    }
                    msg = ws_read.next() => {
                        match msg {
                            Some(Ok(WsMessage::Text(text))) => {
                                match serde_json::from_str::<Value>(&text) {
                                    Ok(frame) => {
                                        should_reconnect =
                                            self.handle_ws_message(frame, &tx).await;
                                        if should_reconnect {
                                            break;
                                        }
                                    }
                                    Err(err) => {
                                        tracing::warn!(
                                            "[wecom_ws] WS frame parse error: {err:#}"
                                        );
                                    }
                                }
                            }
                            Some(Ok(WsMessage::Close(_))) => {
                                tracing::info!("[wecom_ws] WebSocket closed by server");
                                break;
                            }
                            Some(Ok(WsMessage::Pong(_) | _)) => {}
                            Some(Err(err)) => {
                                tracing::warn!("[wecom_ws] WS read error: {err:#}");
                                break;
                            }
                            None => {
                                tracing::info!("[wecom_ws] WebSocket stream ended");
                                break;
                            }
                        }
                    }
                }
            }

            // Disconnect cleanup
            *self.ws_tx.lock().await = None;
            self.fail_pending_responses("socket disconnected").await;

            if should_reconnect {
                // Server-initiated disconnect — reconnect quickly
                tracing::info!("[wecom_ws] disconnected (server event), reconnecting immediately");
                backoff = WECOM_BACKOFF_INITIAL_SECS;
            } else {
                tracing::info!("[wecom_ws] disconnected, will reconnect in {backoff}s");
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(WECOM_BACKOFF_MAX_SECS);
            }
        }
    }

    async fn health_check(&self) -> bool {
        self.ws_tx.lock().await.is_some()
    }

    fn supports_draft_updates(&self) -> bool {
        self.cfg.stream_mode != StreamMode::Off
    }

    async fn send_draft(&self, message: &SendMessage) -> Result<Option<String>> {
        if self.cfg.stream_mode == StreamMode::Off {
            return Ok(None);
        }

        // thread_ts carries the req_id from handle_msg_callback
        let req_id = message.thread_ts.as_deref().unwrap_or("");
        if req_id.is_empty() {
            return Ok(None);
        }
        let stream_id = next_stream_id();
        self.req_id_map
            .lock()
            .insert(stream_id.clone(), req_id.to_string());

        self.ws_send_respond_msg(
            req_id,
            &stream_id,
            WECOM_STREAM_BOOTSTRAP_CONTENT,
            false,
            &[],
        )
        .await?;
        Ok(Some(stream_id))
    }

    async fn update_draft(&self, _recipient: &str, message_id: &str, content: &str) -> Result<()> {
        let req_id = self
            .req_id_map
            .lock()
            .get(message_id)
            .cloned()
            .unwrap_or_default();
        if req_id.is_empty() {
            return Ok(());
        }
        self.ws_send_respond_msg(&req_id, message_id, content, false, &[])
            .await?;
        Ok(())
    }

    async fn finalize_draft(&self, recipient: &str, message_id: &str, content: &str) -> Result<()> {
        let req_id = self
            .req_id_map
            .lock()
            .get(message_id)
            .cloned()
            .unwrap_or_default();

        let (text_without_images, image_paths) = parse_image_markers(content);
        let images = prepare_stream_images(&image_paths).await;
        let (stream_content, overflow) = split_stream_content_and_overflow(&text_without_images);

        if !req_id.is_empty() {
            self.ws_send_respond_msg(&req_id, message_id, &stream_content, true, &images)
                .await?;
        }

        // Send overflow via aibot_send_msg
        if let Some(extra) = overflow {
            let extra_msg = format!("[\u{8865}\u{5145}\u{6d88}\u{606f}]\n{extra}");
            if let Ok((chat_type, chatid)) = parse_scope(recipient) {
                for chunk in split_markdown_chunks(&extra_msg) {
                    let frame = serde_json::json!({
                        "cmd": "aibot_send_msg",
                        "headers": { "req_id": random_ascii_token(16) },
                        "body": {
                            "chatid": chatid,
                            "chat_type": chat_type,
                            "msgtype": "markdown",
                            "markdown": { "content": chunk }
                        }
                    });
                    let _ = self.ws_send_frame(frame).await;
                }
            }
        }

        // Cleanup req_id mapping
        self.req_id_map.lock().remove(message_id);

        Ok(())
    }

    async fn cancel_draft(&self, _recipient: &str, message_id: &str) -> Result<()> {
        let req_id = self
            .req_id_map
            .lock()
            .get(message_id)
            .cloned()
            .unwrap_or_default();
        if !req_id.is_empty() {
            self.ws_send_respond_msg(&req_id, message_id, "", true, &[])
                .await?;
        }
        self.req_id_map.lock().remove(message_id);
        Ok(())
    }
}

// ── Helper functions ─────────────────────────────────────────────────

fn strip_wecom_padding(input: &[u8]) -> Result<&[u8]> {
    let Some(last) = input.last() else {
        anyhow::bail!("invalid WeCom padding: empty payload");
    };
    let pad_len = *last as usize;
    if pad_len == 0 || pad_len > 32 || pad_len > input.len() {
        anyhow::bail!("invalid WeCom padding length");
    }
    Ok(&input[..input.len() - pad_len])
}

fn is_wecom_data_version_conflict_error(err: &anyhow::Error) -> bool {
    let msg = err.to_string();
    msg.contains("errcode=6000") || msg.contains("data version conflict")
}

fn parse_inbound_payload(payload: Value) -> Result<ParsedInbound> {
    let msg_type = payload
        .get("msgtype")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if msg_type.is_empty() {
        anyhow::bail!("missing msgtype");
    }

    let msg_id = payload
        .get("msgid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let chat_type = payload
        .get("chattype")
        .and_then(Value::as_str)
        .unwrap_or("single")
        .to_string();

    let chat_id = payload
        .get("chatid")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    let sender_userid = payload
        .get("from")
        .and_then(|v| v.get("userid"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    let aibot_id = payload
        .get("aibotid")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    Ok(ParsedInbound {
        msg_id,
        msg_type,
        chat_type,
        chat_id,
        sender_userid,
        aibot_id,
        raw_payload: payload,
    })
}

fn compute_scopes(inbound: &ParsedInbound) -> ScopeDecision {
    let chat_type = inbound.chat_type.to_ascii_lowercase();
    if chat_type == "group" {
        let chat_id = inbound
            .chat_id
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let scope = format!("group--{chat_id}");
        return ScopeDecision {
            conversation_scope: scope,
            shared_group_history: true,
        };
    }

    let scope = format!("user--{}", inbound.sender_userid);
    ScopeDecision {
        conversation_scope: scope,
        shared_group_history: false,
    }
}

fn normalize_wecom_identity(value: &str) -> String {
    value.trim().to_string()
}

fn normalize_wecom_allowlist(entries: Vec<String>) -> Vec<String> {
    entries
        .into_iter()
        .map(|entry| normalize_wecom_identity(&entry))
        .filter(|entry| !entry.is_empty())
        .collect()
}

fn allowlist_matches(allowlist: &[String], candidate: &str) -> bool {
    let candidate = normalize_wecom_identity(candidate);
    !candidate.is_empty()
        && allowlist
            .iter()
            .any(|entry| entry == "*" || entry == &candidate)
}

fn evaluate_access_decision(
    allowed_users: &[String],
    allowed_groups: &[String],
    inbound: &ParsedInbound,
) -> AccessDecision {
    if allowed_users.is_empty() && allowed_groups.is_empty() {
        return AccessDecision::AllowlistMissing;
    }

    if allowlist_matches(allowed_users, &inbound.sender_userid) {
        return AccessDecision::Allowed;
    }

    if inbound.chat_type.eq_ignore_ascii_case("group")
        && inbound
            .chat_id
            .as_deref()
            .is_some_and(|chat_id| allowlist_matches(allowed_groups, chat_id))
    {
        return AccessDecision::Allowed;
    }

    AccessDecision::Denied
}

fn build_access_denied_message(inbound: &ParsedInbound, decision: AccessDecision) -> String {
    let userid = normalize_wecom_identity(&inbound.sender_userid);
    let userid = if userid.is_empty() {
        "unknown"
    } else {
        userid.as_str()
    };

    if inbound.chat_type.eq_ignore_ascii_case("group") {
        let chatid = inbound
            .chat_id
            .as_deref()
            .map(normalize_wecom_identity)
            .filter(|chatid| !chatid.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        return match decision {
            AccessDecision::AllowlistMissing => format!(
                "管理员尚未配置 WeCom allowlist，当前机器人不接收任何群消息。\n\n群 chatid: {chatid}\n发送者 userid: {userid}\n\n请在 channels_config.wecom_ws.allowed_groups 或 channels_config.wecom_ws.allowed_users 中加入允许项，也可以临时设置为 [\"*\"] 进行测试。"
            ),
            AccessDecision::Denied => format!(
                "当前群未被允许使用此机器人。\n\n群 chatid: {chatid}\n发送者 userid: {userid}\n\n请管理员将该群加入 channels_config.wecom_ws.allowed_groups，或将你的 userid 加入 channels_config.wecom_ws.allowed_users。"
            ),
            AccessDecision::Allowed => String::new(),
        };
    }

    match decision {
        AccessDecision::AllowlistMissing => format!(
            "管理员尚未配置 WeCom allowlist，当前机器人不接收任何消息。\n\n你的 userid: {userid}\n\n请在 channels_config.wecom_ws.allowed_users 中加入允许项，也可以临时设置为 [\"*\"] 进行测试。"
        ),
        AccessDecision::Denied => format!(
            "你没有权限使用此机器人。\n\n你的 userid: {userid}\n\n请管理员将你的 userid 加入 channels_config.wecom_ws.allowed_users。"
        ),
        AccessDecision::Allowed => String::new(),
    }
}

/// Compose content for framework: quote context (if any) + normalized user text.
/// Sender prefix and static context are handled by the framework (mod.rs).
fn compose_content_for_framework(inbound: &ParsedInbound, normalized: &str) -> String {
    let quote_context = extract_quote_context(&inbound.raw_payload);
    match quote_context {
        Some(quote) => format!("{quote}\n\n{normalized}"),
        None => normalized.to_string(),
    }
}

fn normalize_scope_component(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Parse scope string into (chat_type, chatid) for aibot_send_msg.
/// `user--{userid}` → (1, userid), `group--{chatid}` → (2, chatid)
fn parse_scope(scope: &str) -> Result<(u32, &str)> {
    if let Some(userid) = scope.strip_prefix("user--") {
        Ok((1, userid))
    } else if let Some(chatid) = scope.strip_prefix("group--") {
        Ok((2, chatid))
    } else {
        anyhow::bail!("WeCom: invalid scope format: {scope}")
    }
}

fn summarize_attachment_url_for_log(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return "empty-url".to_string();
    }
    match reqwest::Url::parse(trimmed) {
        Ok(parsed) => {
            let host = parsed.host_str().unwrap_or("unknown-host");
            let query_state = if parsed.query().is_some() {
                "query=present"
            } else {
                "query=none"
            };
            format!(
                "{}://{}{} ({query_state})",
                parsed.scheme(),
                host,
                parsed.path()
            )
        }
        Err(_) => format!("invalid-url(len={})", trimmed.len()),
    }
}

fn truncate_for_log(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let prefix: String = input.chars().take(max_chars).collect();
    format!("{prefix}...(truncated)")
}

fn log_attachment_processing_failure(
    stage: &str,
    err: &anyhow::Error,
    inbound: &ParsedInbound,
    kind: AttachmentKind,
    url: &str,
) {
    tracing::warn!(
        msg_id = %inbound.msg_id,
        msg_type = %inbound.msg_type,
        chat_type = %inbound.chat_type,
        chat_id = %inbound.chat_id.as_deref().unwrap_or("single"),
        sender_userid = %inbound.sender_userid,
        attachment_kind = %kind.as_str(),
        url_target = %summarize_attachment_url_for_log(url),
        error = %format_args!("{err:#}"),
        "{stage}"
    );
}

fn random_emoji() -> &'static str {
    let idx = rand::rng().random_range(0..WECOM_EMOJIS.len());
    WECOM_EMOJIS[idx]
}

fn random_ascii_token(len: usize) -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut out = String::with_capacity(len);
    let mut rng = rand::rng();
    for _ in 0..len {
        let idx = rng.random_range(0..CHARSET.len());
        out.push(CHARSET[idx] as char);
    }
    out
}

fn next_stream_id() -> String {
    format!("zs_{}", random_ascii_token(20))
}

fn contains_stop_command(text: &str) -> bool {
    text.contains("\u{505c}\u{6b62}") || text.to_ascii_lowercase().contains("stop")
}

fn is_clear_session_command(text: &str) -> bool {
    let stripped = strip_edge_mentions(text);
    stripped.eq_ignore_ascii_case("/clear") || stripped.eq_ignore_ascii_case("/new")
}

fn extract_runtime_model_switch_command(text: &str) -> Option<String> {
    let stripped = strip_edge_mentions(text);
    if stripped.is_empty() || !stripped.starts_with('/') {
        return None;
    }

    let command_token = stripped.split_whitespace().next()?;
    let base_command = command_token.split('@').next().unwrap_or(command_token);
    if base_command.eq_ignore_ascii_case("/model") || base_command.eq_ignore_ascii_case("/models") {
        Some(stripped)
    } else {
        None
    }
}

fn strip_edge_mentions(text: &str) -> String {
    let s = text.trim();
    if s.is_empty() {
        return String::new();
    }

    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut start = 0usize;
    loop {
        while start < len && bytes[start].is_ascii_whitespace() {
            start += 1;
        }
        if start >= len || bytes[start] != b'@' {
            break;
        }
        start += 1;
        while start < len && !bytes[start].is_ascii_whitespace() {
            start += 1;
        }
    }

    let mut end = len;
    loop {
        while end > start && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        if end <= start {
            break;
        }
        let mut probe = end;
        while probe > start && !bytes[probe - 1].is_ascii_whitespace() && bytes[probe - 1] != b'@' {
            probe -= 1;
        }
        if probe > start && bytes[probe - 1] == b'@' {
            end = probe - 1;
        } else {
            break;
        }
    }

    s[start..end].trim().to_string()
}

fn extract_stop_signal_text(inbound: &ParsedInbound) -> Option<String> {
    match inbound.msg_type.as_str() {
        "text" => inbound
            .raw_payload
            .get("text")
            .and_then(|v| v.get("content"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned),
        "voice" => inbound
            .raw_payload
            .get("voice")
            .and_then(|v| v.get("content"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned),
        "mixed" => {
            let mut texts = Vec::new();
            let items = inbound
                .raw_payload
                .get("mixed")
                .and_then(|v| v.get("msg_item"))
                .and_then(Value::as_array)?;
            for item in items {
                if item
                    .get("msgtype")
                    .and_then(Value::as_str)
                    .is_some_and(|v| v == "text")
                {
                    if let Some(content) = item
                        .get("text")
                        .and_then(|v| v.get("content"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                    {
                        texts.push(content.to_string());
                    }
                }
            }
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        }
        _ => None,
    }
}

fn inbound_content_preview(inbound: &ParsedInbound) -> String {
    if let Some(text) = extract_stop_signal_text(inbound) {
        return text;
    }

    match inbound.msg_type.as_str() {
        "image" => "[Image message]".to_string(),
        "file" => inbound
            .raw_payload
            .get("file")
            .and_then(|v| v.get("filename"))
            .and_then(Value::as_str)
            .map(|name| format!("[File message: {name}]"))
            .unwrap_or_else(|| "[File message]".to_string()),
        "event" => "[Event callback]".to_string(),
        other => format!("[{other} message]"),
    }
}

fn trim_utf8_to_max_bytes(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_string();
    }
    let mut out = String::new();
    for ch in input.chars() {
        if out.len() + ch.len_utf8() > max_bytes {
            break;
        }
        out.push(ch);
    }
    out
}

fn normalize_stream_content(input: &str) -> String {
    trim_utf8_to_max_bytes(input, WECOM_MARKDOWN_MAX_BYTES)
}

fn split_stream_content_and_overflow(input: &str) -> (String, Option<String>) {
    if input.len() <= WECOM_MARKDOWN_MAX_BYTES {
        return (input.to_string(), None);
    }

    let mut head = String::new();
    let mut tail = String::new();
    let mut overflow = false;
    for ch in input.chars() {
        if !overflow && head.len() + ch.len_utf8() <= WECOM_MARKDOWN_MAX_BYTES {
            head.push(ch);
        } else {
            overflow = true;
            tail.push(ch);
        }
    }

    if tail.is_empty() {
        (head, None)
    } else {
        (head, Some(tail))
    }
}

fn parse_image_markers(text: &str) -> (String, Vec<String>) {
    let mut cleaned = String::new();
    let mut paths = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[IMAGE:") {
        cleaned.push_str(&rest[..start]);
        let after_tag = &rest[start + 7..];
        if let Some(end) = after_tag.find(']') {
            let path = after_tag[..end].trim();
            if !path.is_empty() {
                paths.push(path.to_string());
            }
            rest = &after_tag[end + 1..];
        } else {
            cleaned.push_str(&rest[start..start + 7]);
            rest = after_tag;
        }
    }
    cleaned.push_str(rest);
    let cleaned = cleaned
        .lines()
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    (cleaned, paths)
}

async fn prepare_stream_images(paths: &[String]) -> Vec<StreamImageItem> {
    let mut items = Vec::new();
    for path_str in paths.iter().take(WECOM_STREAM_MAX_IMAGES) {
        let path = Path::new(path_str);
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(ext.as_str(), "jpg" | "jpeg" | "png") {
            tracing::warn!(
                "WeCom stream image skipped (unsupported extension): {}",
                path_str
            );
            continue;
        }
        let data = match tokio::fs::read(path).await {
            Ok(d) => d,
            Err(err) => {
                tracing::warn!(
                    "WeCom stream image read failed: {} \u{2014} {err:#}",
                    path_str
                );
                continue;
            }
        };
        if data.len() > WECOM_IMAGE_MAX_BYTES {
            tracing::warn!(
                "WeCom stream image skipped (too large: {} bytes): {}",
                data.len(),
                path_str
            );
            continue;
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
        let digest = md5_crate::compute(&data);
        let md5_hex = format!("{:x}", digest);
        items.push(StreamImageItem {
            base64: b64,
            md5: md5_hex,
        });
    }
    items
}

fn parse_event_type(payload: &Value) -> Option<String> {
    payload
        .get("event")
        .and_then(|v| v.get("eventtype"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
}

fn extract_template_card_event_key(payload: &Value) -> Option<String> {
    payload
        .get("event")
        .and_then(|v| v.get("template_card_event"))
        .and_then(|v| {
            v.get("event_key")
                .or_else(|| v.get("eventkey"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
}

fn extract_feedback_event_summary(payload: &Value) -> Option<String> {
    let feedback = payload.get("event")?.get("feedback_event")?;
    let feedback_id = feedback
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("-");
    let feedback_type = feedback
        .get("type")
        .and_then(Value::as_i64)
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let content = feedback
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("-");
    Some(format!(
        "feedback_id={feedback_id} feedback_type={feedback_type} content={content}"
    ))
}

fn extract_quote_context(payload: &Value) -> Option<String> {
    let quote = payload.get("quote")?;
    let quote_type = quote
        .get("msgtype")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())?;

    let content = match quote_type {
        "text" => quote
            .get("text")
            .and_then(|v| v.get("content"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| "[\u{5f15}\u{7528}\u{6587}\u{672c}\u{4e3a}\u{7a7a}]".to_string()),
        "voice" => quote
            .get("voice")
            .and_then(|v| v.get("content"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| format!("[\u{5f15}\u{7528}\u{8bed}\u{97f3}\u{8f6c}\u{5199}] {v}"))
            .unwrap_or_else(|| {
                "[\u{5f15}\u{7528}\u{8bed}\u{97f3}\u{65e0}\u{8f6c}\u{5199}]".to_string()
            }),
        "image" => quote
            .get("image")
            .and_then(|v| v.get("local_path"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| format!("[\u{5f15}\u{7528}\u{56fe}\u{7247}] {v}"))
            .unwrap_or_else(|| "[\u{5f15}\u{7528}\u{56fe}\u{7247}]".to_string()),
        "file" => quote
            .get("file")
            .and_then(|v| v.get("local_path"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| format!("[\u{5f15}\u{7528}\u{6587}\u{4ef6}] {v}"))
            .unwrap_or_else(|| "[\u{5f15}\u{7528}\u{6587}\u{4ef6}]".to_string()),
        "mixed" => {
            let mut parts = Vec::new();
            if let Some(items) = quote
                .get("mixed")
                .and_then(|v| v.get("msg_item"))
                .and_then(Value::as_array)
            {
                for item in items {
                    let item_type = item
                        .get("msgtype")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if item_type == "text" {
                        if let Some(text) = item
                            .get("text")
                            .and_then(|v| v.get("content"))
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|v| !v.is_empty())
                        {
                            parts.push(text.to_string());
                        }
                    } else if item_type == "image" {
                        if let Some(path) = item
                            .get("image")
                            .and_then(|v| v.get("local_path"))
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|v| !v.is_empty())
                        {
                            parts.push(format!("[\u{5f15}\u{7528}\u{56fe}\u{7247}] {path}"));
                        } else {
                            parts.push("[\u{5f15}\u{7528}\u{56fe}\u{7247}]".to_string());
                        }
                    }
                }
            }

            if parts.is_empty() {
                "[\u{5f15}\u{7528}\u{56fe}\u{6587}\u{6d88}\u{606f}]".to_string()
            } else {
                parts.join("\n")
            }
        }
        _ => format!("[\u{5f15}\u{7528}\u{6d88}\u{606f} type={quote_type}]"),
    };

    let content = trim_utf8_to_max_bytes(&content, 4_096);
    Some(format!(
        "[WECOM_QUOTE]\nmsgtype={quote_type}\ncontent={content}\n[/WECOM_QUOTE]"
    ))
}

fn bytes_timestamp_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn split_markdown_chunks(input: &str) -> Vec<String> {
    if input.is_empty() {
        return vec![String::new()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for line in input.lines() {
        let candidate = if current.is_empty() {
            line.to_string()
        } else {
            format!("{current}\n{line}")
        };

        if candidate.len() > WECOM_MARKDOWN_CHUNK_BYTES
            && !current.is_empty()
            && current.len() <= WECOM_MARKDOWN_MAX_BYTES
        {
            chunks.push(current);
            current = line.to_string();
            continue;
        }

        current = candidate;
    }

    if !current.is_empty() {
        if current.len() <= WECOM_MARKDOWN_MAX_BYTES {
            chunks.push(current);
        } else {
            let mut buf = String::new();
            for ch in current.chars() {
                if buf.len() + ch.len_utf8() > WECOM_MARKDOWN_CHUNK_BYTES {
                    chunks.push(buf);
                    buf = String::new();
                }
                buf.push(ch);
            }
            if !buf.is_empty() {
                chunks.push(buf);
            }
        }
    }

    if chunks.is_empty() {
        chunks.push(String::new());
    }

    chunks
}

fn is_model_supported_msgtype(msg_type: &str) -> bool {
    matches!(msg_type, "text" | "voice" | "image" | "file" | "mixed")
}

fn is_voice_without_transcript(inbound: &ParsedInbound) -> bool {
    if inbound.msg_type != "voice" {
        return false;
    }
    inbound
        .raw_payload
        .get("voice")
        .and_then(|v| v.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
}

async fn cleanup_inbox_files(root: PathBuf, retention: Duration) {
    if !root.exists() {
        return;
    }

    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
            continue;
        };

        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            let Ok(meta) = entry.metadata().await else {
                continue;
            };

            if meta.is_dir() {
                stack.push(path);
                continue;
            }

            let Ok(modified) = meta.modified() else {
                continue;
            };

            let age = SystemTime::now()
                .duration_since(modified)
                .unwrap_or_else(|_| Duration::from_secs(0));
            if age > retention {
                let _ = tokio::fs::remove_file(&path).await;
            }
        }
    }
}

/// Find the largest char boundary <= `max_bytes` in `s`.
fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut pos = max_bytes;
    while pos > 0 && !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_uses_group_shared_mode_by_default_for_group_chat() {
        let inbound = ParsedInbound {
            msg_id: "m1".to_string(),
            msg_type: "text".to_string(),
            chat_type: "group".to_string(),
            chat_id: Some("g1".to_string()),
            sender_userid: "u1".to_string(),
            aibot_id: "b1".to_string(),
            raw_payload: serde_json::json!({}),
        };

        let scopes = compute_scopes(&inbound);
        assert_eq!(scopes.conversation_scope, "group--g1");
        assert!(scopes.shared_group_history);
    }

    #[test]
    fn split_markdown_chunks_preserves_large_input() {
        let input = "a".repeat(WECOM_MARKDOWN_CHUNK_BYTES * 3 + 100);
        let chunks = split_markdown_chunks(&input);
        assert!(chunks.len() >= 3);
        for chunk in chunks {
            assert!(chunk.len() <= WECOM_MARKDOWN_MAX_BYTES);
        }
    }

    #[test]
    fn split_markdown_chunks_small_input() {
        let input = "Hello WeCom!";
        let chunks = split_markdown_chunks(input);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "Hello WeCom!");
    }

    #[test]
    fn split_markdown_chunks_empty_input() {
        let chunks = split_markdown_chunks("");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "");
    }

    #[test]
    fn summarize_attachment_url_for_log_redacts_query_string() {
        let url = "https://wework.qpic.cn/wwpic/123456/0?auth=secret_token&expires=123";
        let summary = summarize_attachment_url_for_log(url);
        assert_eq!(
            summary,
            "https://wework.qpic.cn/wwpic/123456/0 (query=present)"
        );
        assert!(!summary.contains("secret_token"));
    }

    #[test]
    fn summarize_attachment_url_for_log_handles_invalid_input() {
        let summary = summarize_attachment_url_for_log("not a url");
        assert_eq!(summary, "invalid-url(len=9)");
    }

    #[test]
    fn stop_command_detection_supports_cn_and_en() {
        assert!(contains_stop_command("\u{505c}\u{6b62}"));
        assert!(contains_stop_command("Please STOP now"));
        assert!(!contains_stop_command("\u{7ee7}\u{7eed}\u{5904}\u{7406}"));
    }

    #[test]
    fn parse_event_type_extracts_enter_chat() {
        let payload = serde_json::json!({
            "event": {
                "eventtype": "enter_chat"
            }
        });
        assert_eq!(parse_event_type(&payload).as_deref(), Some("enter_chat"));
    }

    #[test]
    fn extract_quote_context_from_text_quote() {
        let payload = serde_json::json!({
            "quote": {
                "msgtype": "text",
                "text": {
                    "content": "  \u{5f15}\u{7528}\u{5185}\u{5bb9}  "
                }
            }
        });

        let quote = extract_quote_context(&payload).expect("quote should be extracted");
        assert!(quote.contains("msgtype=text"));
        assert!(quote.contains("content=\u{5f15}\u{7528}\u{5185}\u{5bb9}"));
    }

    #[test]
    fn extract_quote_context_from_mixed_quote() {
        let payload = serde_json::json!({
            "quote": {
                "msgtype": "mixed",
                "mixed": {
                    "msg_item": [
                        {
                            "msgtype": "text",
                            "text": {
                                "content": "\u{7b2c}\u{4e00}\u{6bb5}"
                            }
                        },
                        {
                            "msgtype": "image",
                            "image": {
                                "url": "https://example.com/image.png"
                            }
                        }
                    ]
                }
            }
        });

        let quote = extract_quote_context(&payload).expect("quote should be extracted");
        assert!(quote.contains("\u{7b2c}\u{4e00}\u{6bb5}"));
        assert!(quote.contains("\u{5f15}\u{7528}\u{56fe}\u{7247}"));
    }

    #[test]
    fn extract_quote_context_does_not_leak_remote_media_url() {
        let payload = serde_json::json!({
            "quote": {
                "msgtype": "image",
                "image": {
                    "url": "https://example.com/tmp-sign-url"
                }
            }
        });

        let quote = extract_quote_context(&payload).expect("quote should be extracted");
        assert!(quote.contains("[\u{5f15}\u{7528}\u{56fe}\u{7247}]"));
        assert!(!quote.contains("example.com/tmp-sign-url"));
    }

    #[test]
    fn extract_template_card_event_key_reads_event_key() {
        let payload = serde_json::json!({
            "event": {
                "eventtype": "template_card_event",
                "template_card_event": {
                    "event_key": "button_confirm"
                }
            }
        });
        assert_eq!(
            extract_template_card_event_key(&payload).as_deref(),
            Some("button_confirm")
        );
    }

    #[test]
    fn extract_feedback_event_summary_reads_fields() {
        let payload = serde_json::json!({
            "event": {
                "eventtype": "feedback_event",
                "feedback_event": {
                    "id": "fb_1",
                    "type": 2,
                    "content": "not accurate"
                }
            }
        });
        let summary = extract_feedback_event_summary(&payload).expect("summary should exist");
        assert!(summary.contains("feedback_id=fb_1"));
        assert!(summary.contains("feedback_type=2"));
        assert!(summary.contains("content=not accurate"));
    }

    #[test]
    fn parse_image_markers_extracts_paths() {
        let input = "\u{5206}\u{6790}\u{7ed3}\u{679c}:\n[IMAGE:/tmp/chart.png]\n\u{8bf7}\u{53c2}\u{8003}\u{3002}";
        let (cleaned, paths) = parse_image_markers(input);
        assert_eq!(paths, vec!["/tmp/chart.png"]);
        assert!(cleaned.contains("\u{5206}\u{6790}\u{7ed3}\u{679c}:"));
        assert!(cleaned.contains("\u{8bf7}\u{53c2}\u{8003}\u{3002}"));
        assert!(!cleaned.contains("[IMAGE:"));
    }

    #[test]
    fn parse_image_markers_preserves_non_image_tags() {
        let input = "Hello [TOOL:abc] world [IMAGE:/a.jpg] end";
        let (cleaned, paths) = parse_image_markers(input);
        assert_eq!(paths, vec!["/a.jpg"]);
        assert!(cleaned.contains("[TOOL:abc]"));
        assert!(!cleaned.contains("[IMAGE:"));
    }

    #[test]
    fn parse_image_markers_no_markers() {
        let input = "No images here.";
        let (cleaned, paths) = parse_image_markers(input);
        assert_eq!(cleaned, "No images here.");
        assert!(paths.is_empty());
    }

    #[test]
    fn clear_session_bare_commands() {
        assert!(is_clear_session_command("/clear"));
        assert!(is_clear_session_command("/new"));
        assert!(is_clear_session_command("/CLEAR"));
        assert!(is_clear_session_command("/New"));
        assert!(is_clear_session_command("  /clear  "));
    }

    #[test]
    fn clear_session_with_mentions() {
        assert!(is_clear_session_command("@bot /clear"));
        assert!(is_clear_session_command("/clear @bot"));
        assert!(is_clear_session_command("@bot1 @bot2 /new"));
        assert!(is_clear_session_command("@bot /new @other"));
    }

    #[test]
    fn clear_session_rejects_old_and_invalid() {
        assert!(!is_clear_session_command("\u{65b0}\u{4f1a}\u{8bdd}"));
        assert!(!is_clear_session_command("clear history"));
        assert!(!is_clear_session_command("/clear now"));
        assert!(!is_clear_session_command("please /new"));
        assert!(!is_clear_session_command(""));
        assert!(!is_clear_session_command("   "));
    }

    #[test]
    fn runtime_model_switch_command_with_mentions() {
        assert_eq!(
            extract_runtime_model_switch_command("@bot /model gpt-5 @other"),
            Some("/model gpt-5".to_string())
        );
        assert_eq!(
            extract_runtime_model_switch_command("@bot /models openrouter"),
            Some("/models openrouter".to_string())
        );
        assert_eq!(
            extract_runtime_model_switch_command(" /MODEL@zeroclaw qwen-max "),
            Some("/MODEL@zeroclaw qwen-max".to_string())
        );
    }

    #[test]
    fn runtime_model_switch_command_rejects_non_commands() {
        assert_eq!(extract_runtime_model_switch_command("/new"), None);
        assert_eq!(
            extract_runtime_model_switch_command("please /model gpt-5"),
            None
        );
        assert_eq!(extract_runtime_model_switch_command(""), None);
    }

    #[test]
    fn floor_char_boundary_handles_multibyte() {
        let s = "Hello \u{4f60}\u{597d}\u{4e16}\u{754c}";
        let boundary = floor_char_boundary(s, 8);
        assert!(s.is_char_boundary(boundary));
        assert!(boundary <= 8);
        assert!(boundary == 6 || boundary == 9);
    }

    #[test]
    fn floor_char_boundary_full_string() {
        let s = "Hello";
        let boundary = floor_char_boundary(s, 100);
        assert_eq!(boundary, s.len());
    }

    #[test]
    fn parse_scope_user() {
        let (chat_type, chatid) = parse_scope("user--zeroclaw_user").unwrap();
        assert_eq!(chat_type, 1);
        assert_eq!(chatid, "zeroclaw_user");
    }

    #[test]
    fn parse_scope_group() {
        let (chat_type, chatid) = parse_scope("group--zeroclaw_group").unwrap();
        assert_eq!(chat_type, 2);
        assert_eq!(chatid, "zeroclaw_group");
    }

    #[test]
    fn parse_scope_invalid() {
        assert!(parse_scope("invalid_scope").is_err());
    }

    fn test_inbound(chat_type: &str, chat_id: Option<&str>, sender_userid: &str) -> ParsedInbound {
        ParsedInbound {
            msg_id: "msg-1".to_string(),
            msg_type: "text".to_string(),
            chat_type: chat_type.to_string(),
            chat_id: chat_id.map(str::to_string),
            sender_userid: sender_userid.to_string(),
            aibot_id: "bot123".to_string(),
            raw_payload: serde_json::json!({
                "msgtype": "text",
                "msgid": "msg-1",
                "chattype": chat_type,
                "chatid": chat_id,
                "from": { "userid": sender_userid },
                "text": { "content": "@bot hello" }
            }),
        }
    }

    fn test_wecom_ws_config() -> crate::config::schema::WeComWsConfig {
        crate::config::schema::WeComWsConfig {
            bot_id: "bot123".to_string(),
            secret: "secret456".to_string(),
            allowed_users: vec![],
            allowed_groups: vec![],
            file_retention_days: 3,
            max_file_size_mb: 20,
            history_max_turns: 50,
            stream_mode: StreamMode::Partial,
        }
    }

    #[test]
    fn access_decision_denies_when_allowlists_missing() {
        let inbound = test_inbound("single", None, "zeroclaw_user");
        assert_eq!(
            evaluate_access_decision(&[], &[], &inbound),
            AccessDecision::AllowlistMissing
        );
    }

    #[test]
    fn access_decision_allows_userid_in_single_chat() {
        let inbound = test_inbound("single", None, "zeroclaw_user");
        assert_eq!(
            evaluate_access_decision(&["zeroclaw_user".to_string()], &[], &inbound),
            AccessDecision::Allowed
        );
    }

    #[test]
    fn access_decision_allows_group_chatid() {
        let inbound = test_inbound("group", Some("zeroclaw_group"), "zeroclaw_user");
        assert_eq!(
            evaluate_access_decision(&[], &["zeroclaw_group".to_string()], &inbound),
            AccessDecision::Allowed
        );
    }

    #[test]
    fn access_decision_allows_wildcards() {
        let inbound = test_inbound("group", Some("zeroclaw_group"), "zeroclaw_user");
        assert_eq!(
            evaluate_access_decision(&["*".to_string()], &[], &inbound),
            AccessDecision::Allowed
        );
        assert_eq!(
            evaluate_access_decision(&[], &["*".to_string()], &inbound),
            AccessDecision::Allowed
        );
    }

    #[test]
    fn denied_group_message_mentions_chatid_and_userid() {
        let inbound = test_inbound("group", Some("zeroclaw_group"), "zeroclaw_user");
        let text = build_access_denied_message(&inbound, AccessDecision::Denied);
        assert!(text.contains("zeroclaw_group"));
        assert!(text.contains("zeroclaw_user"));
        assert!(text.contains("allowed_groups"));
        assert!(text.contains("wecom_ws"));
    }

    #[test]
    fn supports_draft_updates_respects_stream_mode() {
        let mut off_cfg = test_wecom_ws_config();
        off_cfg.stream_mode = StreamMode::Off;
        let off = WeComWsChannel::new(&off_cfg, Path::new("/tmp")).unwrap();
        assert!(!off.supports_draft_updates());

        let partial = WeComWsChannel::new(&test_wecom_ws_config(), Path::new("/tmp")).unwrap();
        assert!(partial.supports_draft_updates());
    }

    #[tokio::test]
    async fn send_draft_returns_none_when_stream_mode_off() {
        let mut cfg = test_wecom_ws_config();
        cfg.stream_mode = StreamMode::Off;
        let channel = WeComWsChannel::new(&cfg, Path::new("/tmp")).unwrap();

        let id = channel
            .send_draft(&SendMessage::new("draft", "user--zeroclaw_user"))
            .await
            .unwrap();

        assert!(id.is_none());
    }

    #[tokio::test]
    async fn send_with_req_id_uses_respond_msg_when_stream_mode_off() {
        let mut cfg = test_wecom_ws_config();
        cfg.stream_mode = StreamMode::Off;
        let channel = WeComWsChannel::new(&cfg, Path::new("/tmp")).unwrap();

        let (ws_tx, mut ws_rx) = mpsc::channel::<WsOutbound>(4);
        *channel.ws_tx.lock().await = Some(ws_tx);

        let responder_channel = channel.clone();
        let responder = tokio::spawn(async move {
            let Some(WsOutbound::Frame(frame)) = ws_rx.recv().await else {
                panic!("expected respond_msg frame");
            };
            let req_id = frame
                .get("headers")
                .and_then(|headers| headers.get("req_id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            responder_channel
                .maybe_handle_command_response(&serde_json::json!({
                    "headers": { "req_id": req_id },
                    "errcode": 0,
                    "errmsg": "ok"
                }))
                .await;
            frame
        });

        channel
            .send(
                &SendMessage::new("runtime ok", "user--zeroclaw_user")
                    .in_thread(Some("req-runtime".to_string())),
            )
            .await
            .unwrap();

        let frame = responder.await.unwrap();
        assert_eq!(
            frame.get("cmd").and_then(Value::as_str),
            Some("aibot_respond_msg")
        );
        assert_eq!(
            frame
                .get("headers")
                .and_then(|headers| headers.get("req_id"))
                .and_then(Value::as_str),
            Some("req-runtime")
        );
        assert_eq!(
            frame
                .pointer("/body/stream/content")
                .and_then(Value::as_str),
            Some("runtime ok")
        );
        assert_eq!(
            frame
                .pointer("/body/stream/finish")
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[tokio::test]
    async fn send_without_req_id_uses_send_msg() {
        let channel = WeComWsChannel::new(&test_wecom_ws_config(), Path::new("/tmp")).unwrap();

        let (ws_tx, mut ws_rx) = mpsc::channel::<WsOutbound>(4);
        *channel.ws_tx.lock().await = Some(ws_tx);

        let responder_channel = channel.clone();
        let responder = tokio::spawn(async move {
            let Some(WsOutbound::Frame(frame)) = ws_rx.recv().await else {
                panic!("expected send_msg frame");
            };
            let req_id = frame
                .get("headers")
                .and_then(|headers| headers.get("req_id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            responder_channel
                .maybe_handle_command_response(&serde_json::json!({
                    "headers": { "req_id": req_id },
                    "errcode": 0,
                    "errmsg": "ok"
                }))
                .await;
            frame
        });

        channel
            .send(&SendMessage::new("hello proactive", "user--zeroclaw_user"))
            .await
            .unwrap();

        let frame = responder.await.unwrap();
        assert_eq!(
            frame.get("cmd").and_then(Value::as_str),
            Some("aibot_send_msg")
        );
        assert_eq!(
            frame
                .pointer("/body/markdown/content")
                .and_then(Value::as_str),
            Some("hello proactive")
        );
    }

    #[tokio::test]
    async fn command_response_resolves_waiter_successfully() {
        let config = test_wecom_ws_config();
        let channel = WeComWsChannel::new(&config, Path::new("/tmp")).unwrap();

        let (waiter, rx) = tokio::sync::oneshot::channel();
        channel
            .pending_responses
            .lock()
            .await
            .insert("req-ok".to_string(), waiter);

        assert!(
            channel
                .maybe_handle_command_response(&serde_json::json!({
                    "headers": { "req_id": "req-ok" },
                    "errcode": 0,
                    "errmsg": "ok"
                }))
                .await
        );
        assert!(rx.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn command_response_resolves_waiter_failure() {
        let config = test_wecom_ws_config();
        let channel = WeComWsChannel::new(&config, Path::new("/tmp")).unwrap();

        let (waiter, rx) = tokio::sync::oneshot::channel();
        channel
            .pending_responses
            .lock()
            .await
            .insert("req-fail".to_string(), waiter);

        assert!(
            channel
                .maybe_handle_command_response(&serde_json::json!({
                    "headers": { "req_id": "req-fail" },
                    "errcode": 93001,
                    "errmsg": "session not allowed"
                }))
                .await
        );
        let err = rx.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("errcode=93001"));
        assert!(err.contains("session not allowed"));
    }

    #[tokio::test]
    async fn handle_ws_message_consumes_command_ack_without_forwarding() {
        let config = test_wecom_ws_config();
        let channel = WeComWsChannel::new(&config, Path::new("/tmp")).unwrap();

        let (waiter, ack_rx) = tokio::sync::oneshot::channel();
        channel
            .pending_responses
            .lock()
            .await
            .insert("req-ack".to_string(), waiter);

        let (tx, mut rx) = mpsc::channel::<ChannelMessage>(1);
        let should_reconnect = channel
            .handle_ws_message(
                serde_json::json!({
                    "cmd": "aibot_respond_msg",
                    "headers": { "req_id": "req-ack" },
                    "errcode": 0,
                    "errmsg": "ok"
                }),
                &tx,
            )
            .await;

        assert!(!should_reconnect);
        assert!(ack_rx.await.unwrap().is_ok());
        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "command ack must not be forwarded as an inbound channel message"
        );
    }

    #[tokio::test]
    async fn clear_command_forwards_runtime_new_session_without_immediate_ws_reply() {
        let mut config = test_wecom_ws_config();
        config.allowed_users = vec!["zeroclaw_user".to_string()];
        let channel = WeComWsChannel::new(&config, Path::new("/tmp")).unwrap();

        let (ws_tx, mut ws_rx) = mpsc::channel::<WsOutbound>(1);
        *channel.ws_tx.lock().await = Some(ws_tx);

        let (tx, mut rx) = mpsc::channel::<ChannelMessage>(1);
        channel
            .handle_msg_callback(
                serde_json::json!({
                    "headers": { "req_id": "req-clear" },
                    "body": {
                        "msgtype": "text",
                        "msgid": "msg-clear",
                        "chattype": "single",
                        "from": { "userid": "zeroclaw_user" },
                        "text": { "content": "/clear" }
                    }
                }),
                &tx,
            )
            .await;

        let forwarded = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("clear command should be forwarded promptly")
            .expect("clear command should produce a framework message");
        assert_eq!(forwarded.content, "/new");
        assert_eq!(forwarded.thread_ts.as_deref(), Some("req-clear"));

        assert!(
            tokio::time::timeout(Duration::from_millis(100), ws_rx.recv())
                .await
                .is_err(),
            "clear command should not emit an immediate websocket reply"
        );
    }

    #[tokio::test]
    async fn clear_command_ws_dispatch_does_not_block_when_framework_queue_is_full() {
        let mut config = test_wecom_ws_config();
        config.allowed_users = vec!["zeroclaw_user".to_string()];
        let channel = WeComWsChannel::new(&config, Path::new("/tmp")).unwrap();

        let (tx, mut rx) = mpsc::channel::<ChannelMessage>(1);
        tx.send(ChannelMessage {
            id: "prefill-clear".to_string(),
            sender: "tester".to_string(),
            reply_target: "user--zeroclaw_user".to_string(),
            content: "prefill".to_string(),
            channel: "wecom_ws".to_string(),
            timestamp: bytes_timestamp_now(),
            thread_ts: None,
        })
        .await
        .unwrap();

        let should_reconnect = tokio::time::timeout(
            Duration::from_millis(100),
            channel.handle_ws_message(
                serde_json::json!({
                    "cmd": "aibot_msg_callback",
                    "headers": { "req_id": "req-clear-dispatch" },
                    "body": {
                        "msgtype": "text",
                        "msgid": "msg-clear-dispatch",
                        "chattype": "single",
                        "from": { "userid": "zeroclaw_user" },
                        "text": { "content": "/clear" }
                    }
                }),
                &tx,
            ),
        )
        .await
        .expect("clear dispatch should not block the websocket loop");

        assert!(!should_reconnect);

        let first = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("prefilled framework message should be readable")
            .expect("prefilled framework message should exist");
        assert_eq!(first.id, "prefill-clear");

        let forwarded = tokio::time::timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("clear command should forward once queue space is available")
            .expect("clear command should produce a framework message");
        assert_eq!(forwarded.content, "/new");
        assert_eq!(forwarded.thread_ts.as_deref(), Some("req-clear-dispatch"));
    }

    #[tokio::test]
    async fn unauthorized_group_message_replies_with_chatid_and_does_not_forward() {
        let config = test_wecom_ws_config();
        let channel = WeComWsChannel::new(&config, Path::new("/tmp")).unwrap();

        let (ws_tx, mut ws_rx) = mpsc::channel::<WsOutbound>(4);
        *channel.ws_tx.lock().await = Some(ws_tx);

        let responder_channel = channel.clone();
        let responder = tokio::spawn(async move {
            let Some(WsOutbound::Frame(frame)) = ws_rx.recv().await else {
                panic!("expected access-denied response frame");
            };
            let req_id = frame
                .get("headers")
                .and_then(|headers| headers.get("req_id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let content = frame
                .pointer("/body/stream/content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            responder_channel
                .maybe_handle_command_response(&serde_json::json!({
                    "headers": { "req_id": req_id },
                    "errcode": 0,
                    "errmsg": "ok"
                }))
                .await;
            content
        });

        let (tx, mut rx) = mpsc::channel::<ChannelMessage>(1);
        channel
            .handle_msg_callback(
                serde_json::json!({
                    "headers": { "req_id": "req-denied" },
                    "body": {
                        "msgtype": "text",
                        "msgid": "msg-denied",
                        "chattype": "group",
                        "chatid": "zeroclaw_group",
                        "from": { "userid": "zeroclaw_user" },
                        "text": { "content": "@bot hello" }
                    }
                }),
                &tx,
            )
            .await;

        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "unauthorized message must not reach framework"
        );

        let denied = responder.await.unwrap();
        assert!(denied.contains("zeroclaw_group"));
        assert!(denied.contains("zeroclaw_user"));
        assert!(denied.contains("allowed_groups"));
    }

    #[tokio::test]
    async fn unauthorized_message_ws_dispatch_returns_without_waiting_for_ack() {
        let config = test_wecom_ws_config();
        let channel = WeComWsChannel::new(&config, Path::new("/tmp")).unwrap();

        let (ws_tx, mut ws_rx) = mpsc::channel::<WsOutbound>(4);
        *channel.ws_tx.lock().await = Some(ws_tx);

        let (tx, mut rx) = mpsc::channel::<ChannelMessage>(1);
        let should_reconnect = tokio::time::timeout(
            Duration::from_millis(100),
            channel.handle_ws_message(
                serde_json::json!({
                    "cmd": "aibot_msg_callback",
                    "headers": { "req_id": "req-denied-no-ack" },
                    "body": {
                        "msgtype": "text",
                        "msgid": "msg-denied-no-ack",
                        "chattype": "single",
                        "from": { "userid": "zeroclaw_user" },
                        "text": { "content": "@bot hello" }
                    }
                }),
                &tx,
            ),
        )
        .await
        .expect("access-denied dispatch should not block on websocket ack");

        assert!(!should_reconnect);

        assert!(
            tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "unauthorized message must not reach framework"
        );

        let Some(WsOutbound::Frame(frame)) =
            tokio::time::timeout(Duration::from_millis(100), ws_rx.recv())
                .await
                .expect("access-denied reply should be queued promptly")
        else {
            panic!("expected access-denied response frame");
        };

        assert_eq!(
            frame.get("cmd").and_then(Value::as_str),
            Some("aibot_respond_msg")
        );
        assert_eq!(
            frame
                .get("headers")
                .and_then(|headers| headers.get("req_id"))
                .and_then(Value::as_str),
            Some("req-denied-no-ack")
        );
        assert!(
            frame
                .pointer("/body/stream/content")
                .and_then(Value::as_str)
                .is_some_and(|content| content.contains("allowed_users")),
            "access-denied reply should explain how to configure the allowlist"
        );
    }

    #[tokio::test]
    async fn stream_reply_retries_data_version_conflict() {
        let config = test_wecom_ws_config();
        let channel = WeComWsChannel::new(&config, Path::new("/tmp")).unwrap();

        let (tx, mut rx) = mpsc::channel::<WsOutbound>(8);
        *channel.ws_tx.lock().await = Some(tx);

        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let responder_channel = channel.clone();
        let responder_attempts = Arc::clone(&attempts);
        let responder = tokio::spawn(async move {
            while let Some(WsOutbound::Frame(frame)) = rx.recv().await {
                let attempt = responder_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let req_id = frame
                    .get("headers")
                    .and_then(|headers| headers.get("req_id"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();

                let errcode = if attempt == 0 { 6000 } else { 0 };
                let errmsg = if errcode == 0 {
                    "ok"
                } else {
                    "more than one callers at the same time, data version conflict"
                };
                responder_channel
                    .maybe_handle_command_response(&serde_json::json!({
                        "headers": { "req_id": req_id },
                        "errcode": errcode,
                        "errmsg": errmsg
                    }))
                    .await;

                if errcode == 0 {
                    break;
                }
            }
        });

        channel
            .ws_send_respond_msg("req-stream", "stream-1", "hello", false, &[])
            .await
            .unwrap();

        responder.await.unwrap();
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn stream_reply_serializes_same_req_id_updates() {
        let config = test_wecom_ws_config();
        let channel = WeComWsChannel::new(&config, Path::new("/tmp")).unwrap();

        let (tx, mut rx) = mpsc::channel::<WsOutbound>(8);
        *channel.ws_tx.lock().await = Some(tx);

        let first_channel = channel.clone();
        let first = tokio::spawn(async move {
            first_channel
                .ws_send_respond_msg("req-serial", "stream-1", "first", false, &[])
                .await
        });

        let second_channel = channel.clone();
        let second = tokio::spawn(async move {
            second_channel
                .ws_send_respond_msg("req-serial", "stream-1", "second", false, &[])
                .await
        });

        let first_frame = tokio::time::timeout(Duration::from_millis(250), rx.recv())
            .await
            .expect("first frame should arrive")
            .expect("first frame should exist");
        let WsOutbound::Frame(first_frame) = first_frame;
        assert_eq!(
            first_frame
                .get("body")
                .and_then(|body| body.get("stream"))
                .and_then(|stream| stream.get("content"))
                .and_then(Value::as_str),
            Some("first")
        );

        assert!(
            tokio::time::timeout(Duration::from_millis(75), rx.recv())
                .await
                .is_err(),
            "second frame should wait for the first ack"
        );

        channel
            .maybe_handle_command_response(&serde_json::json!({
                "headers": { "req_id": "req-serial" },
                "errcode": 0,
                "errmsg": "ok"
            }))
            .await;
        first.await.unwrap().unwrap();

        let second_frame = tokio::time::timeout(Duration::from_millis(250), rx.recv())
            .await
            .expect("second frame should arrive after first ack")
            .expect("second frame should exist");
        let WsOutbound::Frame(second_frame) = second_frame;
        assert_eq!(
            second_frame
                .get("body")
                .and_then(|body| body.get("stream"))
                .and_then(|stream| stream.get("content"))
                .and_then(Value::as_str),
            Some("second")
        );

        channel
            .maybe_handle_command_response(&serde_json::json!({
                "headers": { "req_id": "req-serial" },
                "errcode": 0,
                "errmsg": "ok"
            }))
            .await;
        second.await.unwrap().unwrap();
    }
}
