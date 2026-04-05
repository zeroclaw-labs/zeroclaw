use crate::channels::media_pipeline::MediaAttachment;
use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::Deserialize;
use tokio::sync::mpsc;

/// Call `gws` CLI via `zsh -lc` and return parsed JSON output.
/// `params` = URL query params (--params), `body` = request body (--json).
fn gws_call(
    subcmd: &str,
    params: Option<serde_json::Value>,
    body: Option<serde_json::Value>,
) -> anyhow::Result<serde_json::Value> {
    let params_json = params
        .map(|p| serde_json::to_string(&p))
        .transpose()?;
    let body_json = body
        .map(|b| serde_json::to_string(&b))
        .transpose()?;

    let mut cmd = format!("gws {subcmd}");
    if params_json.is_some() {
        cmd.push_str(" --params \"$GWS_PARAMS\"");
    }
    if body_json.is_some() {
        cmd.push_str(" --json \"$GWS_JSON\"");
    }

    let mut command = std::process::Command::new("zsh");
    command.arg("-lc").arg(&cmd);
    if let Some(ref json) = params_json {
        command.env("GWS_PARAMS", json);
    }
    if let Some(ref json) = body_json {
        command.env("GWS_JSON", json);
    }

    let output = command.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gws error: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Ok(serde_json::Value::Object(Default::default()));
    }
    Ok(serde_json::from_str(&stdout)?)
}

/// Download an attachment to /tmp and return the file bytes.
fn gws_download_attachment(resource_name: &str, filename: &str) -> anyhow::Result<Vec<u8>> {
    // gws saves to /tmp/{filename} when --output is used
    let cmd = format!(
        "cd /tmp && GWS_PARAMS={:?} gws chat media download --params \"$GWS_PARAMS\" --output {}",
        serde_json::json!({ "resourceName": resource_name, "alt": "media" }).to_string(),
        filename,
    );

    let output = std::process::Command::new("zsh")
        .arg("-lc")
        .arg(&cmd)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gws download error: {stderr}");
    }

    let path = format!("/tmp/{filename}");
    let bytes = std::fs::read(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(bytes)
}

#[derive(Debug, Deserialize)]
struct GChatMessage {
    name: String,
    #[serde(rename = "createTime")]
    create_time: String,
    text: Option<String>,
    sender: Option<GChatSender>,
    attachment: Option<Vec<GChatAttachment>>,
}

#[derive(Debug, Deserialize)]
struct GChatSender {
    name: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    #[serde(rename = "type")]
    sender_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GChatAttachment {
    #[serde(rename = "contentName")]
    content_name: Option<String>,
    #[serde(rename = "contentType")]
    content_type: Option<String>,
    #[serde(rename = "attachmentDataRef")]
    attachment_data_ref: Option<GChatAttachmentDataRef>,
}

#[derive(Debug, Deserialize)]
struct GChatAttachmentDataRef {
    #[serde(rename = "resourceName")]
    resource_name: String,
}

/// Process a single GChat attachment.
/// Returns (image_marker, audio_attachment) — at most one of the two is Some.
fn process_attachment(
    att: &GChatAttachment,
    idx: usize,
) -> (Option<String>, Option<MediaAttachment>) {
    let resource_name = match att.attachment_data_ref.as_ref().map(|r| r.resource_name.as_str()) {
        Some(r) => r,
        None => return (None, None),
    };

    let content_name = att.content_name.as_deref().unwrap_or("attachment");
    let content_type = att.content_type.as_deref().unwrap_or("");
    let ext = content_name.rsplit('.').next().unwrap_or("bin");
    let filename = format!("zchat-att-{}-{}.{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs(), idx, ext);

    let bytes = match gws_download_attachment(resource_name, &filename) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("gchat: attachment download failed: {e}");
            return (None, None);
        }
    };

    if content_type.starts_with("image/") {
        // Inject as base64 data URI for Bedrock multimodal
        let b64 = BASE64.encode(&bytes);
        let marker = format!("[IMAGE:data:{content_type};base64,{b64}]");
        (Some(marker), None)
    } else if content_type.starts_with("audio/") || content_type.starts_with("video/audio") {
        let media = MediaAttachment {
            file_name: content_name.to_string(),
            data: bytes,
            mime_type: Some(content_type.to_string()),
        };
        (None, Some(media))
    } else {
        tracing::debug!("gchat: skipping unsupported attachment type: {content_type}");
        (None, None)
    }
}

/// Google Chat channel — polls a space via `gws` CLI.
#[derive(Clone)]
pub struct GChatChannel {
    space_id: String,
    allowed_senders: Vec<String>,
    bot_sender_id: Option<String>,
    poll_interval_secs: u64,
}

impl GChatChannel {
    pub fn new(
        space_id: String,
        allowed_senders: Vec<String>,
        bot_sender_id: Option<String>,
        poll_interval_secs: Option<u64>,
    ) -> Self {
        Self {
            space_id,
            allowed_senders,
            bot_sender_id,
            poll_interval_secs: poll_interval_secs.unwrap_or(8),
        }
    }

    fn is_sender_allowed(&self, sender_id: &str) -> bool {
        self.allowed_senders.iter().any(|s| s == sender_id)
    }
}

#[async_trait]
impl Channel for GChatChannel {
    fn name(&self) -> &str {
        "gchat"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let space_id = if message.recipient.starts_with("spaces/") {
            message.recipient.clone()
        } else {
            self.space_id.clone()
        };

        gws_call(
            "chat spaces messages create",
            Some(serde_json::json!({ "parent": space_id })),
            Some(serde_json::json!({ "text": message.content })),
        )?;

        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tracing::info!(space_id = %self.space_id, "Google Chat channel listening...");

        // Start 60s in the past on first poll
        let mut last_timestamp = chrono::Utc::now() - chrono::Duration::seconds(60);
        // Track processed message names to prevent reprocessing within the same second
        let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(self.poll_interval_secs)).await;

            // GChat API filter doesn't support sub-seconds; use last_timestamp truncated to
            // whole seconds. Dedup via seen_names handles messages within the same second.
            let since = last_timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string();

            let space_id = self.space_id.clone();
            let result = tokio::task::spawn_blocking(move || {
                gws_call(
                    "chat spaces messages list",
                    Some(serde_json::json!({
                        "parent": space_id,
                        "filter": format!("createTime > \"{since}\""),
                    })),
                    None,
                )
            })
            .await
            .map_err(|e| anyhow::anyhow!("gchat poll join error: {e}"))??;

            let messages: Vec<GChatMessage> = result
                .get("messages")
                .and_then(|m| serde_json::from_value(m.clone()).ok())
                .unwrap_or_default();

            // Process chronologically
            let mut sorted = messages;
            sorted.sort_by(|a, b| a.create_time.cmp(&b.create_time));

            for msg in sorted {
                // Skip already-processed messages (dedup within the same second)
                if seen_names.contains(&msg.name) {
                    continue;
                }

                // Skip bots
                if msg
                    .sender
                    .as_ref()
                    .and_then(|s| s.sender_type.as_deref())
                    == Some("BOT")
                {
                    continue;
                }

                let sender_id = msg
                    .sender
                    .as_ref()
                    .and_then(|s| s.name.as_deref())
                    .unwrap_or("")
                    .to_string();

                // Skip own bot messages
                if let Some(ref bot_id) = self.bot_sender_id {
                    if &sender_id == bot_id {
                        continue;
                    }
                }

                if !self.is_sender_allowed(&sender_id) {
                    continue;
                }

                // Process attachments: images → base64 markers, audio → MediaAttachment
                let mut image_markers: Vec<String> = Vec::new();
                let mut audio_attachments: Vec<MediaAttachment> = Vec::new();

                for (idx, att) in msg.attachment.as_deref().unwrap_or(&[]).iter().enumerate() {
                    let (img, audio) = process_attachment(att, idx);
                    if let Some(m) = img { image_markers.push(m); }
                    if let Some(a) = audio { audio_attachments.push(a); }
                }

                let text = msg.text.unwrap_or_default();

                // Build content: image markers first, then text
                let content = if image_markers.is_empty() {
                    text.clone()
                } else {
                    let mut c = image_markers.join("\n");
                    if !text.trim().is_empty() {
                        c.push('\n');
                        c.push_str(&text);
                    }
                    c
                };

                // Skip if no content and no attachments
                if content.trim().is_empty() && audio_attachments.is_empty() {
                    continue;
                }

                // Track name and advance timestamp
                seen_names.insert(msg.name.clone());
                if seen_names.len() > 500 {
                    seen_names.clear();
                }
                if let Ok(t) = chrono::DateTime::parse_from_rfc3339(&msg.create_time) {
                    let t_utc = t.with_timezone(&chrono::Utc);
                    if t_utc > last_timestamp {
                        last_timestamp = t_utc;
                    }
                }

                let display_name = msg
                    .sender
                    .as_ref()
                    .and_then(|s| s.display_name.as_deref())
                    .unwrap_or(&sender_id)
                    .to_string();

                let channel_msg = ChannelMessage {
                    id: msg.name.clone(),
                    sender: display_name,
                    reply_target: self.space_id.clone(),
                    content,
                    channel: "gchat".to_string(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    thread_ts: None,
                    interruption_scope_id: None,
                    attachments: audio_attachments,
                };

                if tx.send(channel_msg).await.is_err() {
                    return Ok(());
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        tokio::task::spawn_blocking(|| {
            std::process::Command::new("zsh")
                .arg("-lc")
                .arg("gws --version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false)
    }
}
