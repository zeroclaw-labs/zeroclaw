//! Outbound Discord REST: turn a message (content + future embeds/components,
//! via [`DiscordOutgoing`]) plus optional files into an HTTP request and parse
//! the response. Channel-message + reaction-URL builders only — interaction
//! callbacks (defer/reject/followup) live in `interaction`.

use std::fmt::Write as _;
use std::path::PathBuf;

use reqwest::multipart::{Form, Part};

use super::types::DiscordOutgoing;

/// POST a content-only plain-text message and return the new message's ID.
/// A thin adapter over [`send_discord_message_payload`] for the many callers
/// (non-first chunks, streaming replies, approvals) that send no embeds.
pub(crate) async fn send_discord_message_json(
    client: &reqwest::Client,
    bot_token: &str,
    recipient: &str,
    content: &str,
) -> anyhow::Result<String> {
    send_discord_message_payload(
        client,
        bot_token,
        recipient,
        &DiscordOutgoing::text(content),
    )
    .await
}

/// POST a full message envelope (content plus any embeds) and return the new
/// message's ID. Callers that don't need the ID can discard it.
pub(crate) async fn send_discord_message_payload(
    client: &reqwest::Client,
    bot_token: &str,
    recipient: &str,
    payload: &DiscordOutgoing,
) -> anyhow::Result<String> {
    let url = format!("https://discord.com/api/v10/channels/{recipient}/messages");
    let body = payload.to_rest_json();

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
        anyhow::bail!("Discord send message failed ({status}): {err}");
    }

    extract_message_id(resp).await
}

pub(crate) async fn send_discord_outgoing(
    client: &reqwest::Client,
    bot_token: &str,
    recipient: &str,
    outgoing: &DiscordOutgoing,
) -> anyhow::Result<String> {
    let url = format!("https://discord.com/api/v10/channels/{recipient}/messages");
    let body = outgoing.to_rest_json();

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
        anyhow::bail!("Discord send message failed ({status}): {err}");
    }

    extract_message_id(resp).await
}

/// POST a full message envelope with file attachments via multipart,
/// returning the new message's ID. Callers that don't need the ID can discard it.
pub(crate) async fn send_discord_message_payload_with_files(
    client: &reqwest::Client,
    bot_token: &str,
    recipient: &str,
    payload: &DiscordOutgoing,
    files: &[PathBuf],
) -> anyhow::Result<String> {
    let url = format!("https://discord.com/api/v10/channels/{recipient}/messages");

    let mut form = Form::new().text("payload_json", payload.payload_json());

    for (idx, path) in files.iter().enumerate() {
        let bytes = tokio::fs::read(path).await.map_err(|error| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path.display().to_string(),
                        "phase": "attachment_read",
                        "error": format!("{}", error),
                    })),
                "discord: failed to read attachment"
            );
            anyhow::Error::msg(format!(
                "Discord attachment read failed for '{}': {error}",
                path.display()
            ))
        })?;
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attachment.bin")
            .to_string();
        form = form.part(
            format!("files[{idx}]"),
            Part::bytes(bytes).file_name(filename),
        );
    }

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .multipart(form)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
        anyhow::bail!("Discord send message with files failed ({status}): {err}");
    }

    extract_message_id(resp).await
}

async fn extract_message_id(resp: reqwest::Response) -> anyhow::Result<String> {
    let body: serde_json::Value = resp.json().await?;
    body.get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"field": "id"})),
                "discord: send response missing id field"
            );
            anyhow::Error::msg("Discord send response missing 'id' field")
        })
}

/// Edit an existing Discord message with content only. A thin adapter over
/// [`edit_discord_message_payload`].
pub(crate) async fn edit_discord_message(
    client: &reqwest::Client,
    bot_token: &str,
    channel_id: &str,
    message_id: &str,
    content: &str,
) -> anyhow::Result<()> {
    edit_discord_message_payload(
        client,
        bot_token,
        channel_id,
        message_id,
        &DiscordOutgoing::text(content),
    )
    .await
}

pub(crate) async fn edit_discord_message_payload(
    client: &reqwest::Client,
    bot_token: &str,
    channel_id: &str,
    message_id: &str,
    payload: &DiscordOutgoing,
) -> anyhow::Result<()> {
    let url = format!("https://discord.com/api/v10/channels/{channel_id}/messages/{message_id}");
    let body = payload.to_rest_json();

    let resp = client
        .patch(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .json(&body)
        .send()
        .await?;

    if resp.status().as_u16() == 429 {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "edit message rate-limited (429), skipping update"
        );
        return Ok(());
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
        anyhow::bail!("edit message failed ({status}): {err}");
    }

    Ok(())
}

/// Delete a Discord message.
/// Returns `Ok(())` on success. On HTTP 429 (rate limited), logs at debug
/// level and returns `Ok(())` since a stale message is cosmetic only.
pub(crate) async fn delete_discord_message(
    client: &reqwest::Client,
    bot_token: &str,
    channel_id: &str,
    message_id: &str,
) -> anyhow::Result<()> {
    let url = format!("https://discord.com/api/v10/channels/{channel_id}/messages/{message_id}");

    let resp = client
        .delete(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .send()
        .await?;

    if resp.status().as_u16() == 429 {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "delete message rate-limited (429), skipping"
        );
        return Ok(());
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
        anyhow::bail!("delete message failed ({status}): {err}");
    }

    Ok(())
}

/// A thread Discord created on a message: the thread's channel id and the
/// user Discord records as its creator (the bot, for threads it starts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StartedThread {
    pub(crate) id: String,
    pub(crate) owner_id: Option<String>,
}

/// Discord's answer to a start-thread request that the caller can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ThreadStart {
    Created(StartedThread),
    /// The message already has a thread (JSON error code 160004). Discord gives
    /// a thread started on a message that message's id, so the caller can
    /// reply in it without another request.
    AlreadyExists,
    /// HTTP 403: the bot lacks access or a permission in that channel. Carries
    /// Discord's response body for the log.
    Forbidden(String),
}

/// Discord's JSON error code for "a thread has already been created for this
/// message".
const DISCORD_THREAD_ALREADY_CREATED: u64 = 160_004;

/// Start a thread on an existing guild message
/// (`POST {api_base}/channels/{channel_id}/messages/{message_id}/threads`).
///
/// The auto-archive duration is left to the channel's own default. Statuses
/// the caller handles differently come back as [`ThreadStart`] variants; any
/// other failure is an error carrying Discord's status and body.
pub(crate) async fn start_discord_thread_from_message(
    client: &reqwest::Client,
    api_base: &str,
    bot_token: &str,
    channel_id: &str,
    message_id: &str,
    name: &str,
) -> anyhow::Result<ThreadStart> {
    let url = format!("{api_base}/channels/{channel_id}/messages/{message_id}/threads");
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await?;
    let status = resp.status();
    if status.is_success() {
        let body: serde_json::Value = resp.json().await?;
        let id = body
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::Error::msg("Discord start-thread response missing 'id' field"))?
            .to_string();
        let owner_id = body
            .get("owner_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        return Ok(ThreadStart::Created(StartedThread { id, owner_id }));
    }

    let body = resp
        .text()
        .await
        .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
    if status == reqwest::StatusCode::FORBIDDEN {
        return Ok(ThreadStart::Forbidden(body));
    }
    let code = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("code").and_then(serde_json::Value::as_u64));
    if code == Some(DISCORD_THREAD_ALREADY_CREATED) {
        return Ok(ThreadStart::AlreadyExists);
    }
    anyhow::bail!("start thread failed ({status}): {body}")
}

pub(crate) fn encode_emoji_for_discord(emoji: &str) -> String {
    if emoji.contains(':') {
        return emoji.to_string();
    }

    let mut encoded = String::new();
    for byte in emoji.as_bytes() {
        let _ = write!(encoded, "%{byte:02X}");
    }
    encoded
}

pub(crate) fn discord_reaction_url(channel_id: &str, message_id: &str, emoji: &str) -> String {
    let raw_id = message_id.strip_prefix("discord_").unwrap_or(message_id);
    let encoded_emoji = encode_emoji_for_discord(emoji);
    format!(
        "https://discord.com/api/v10/channels/{channel_id}/messages/{raw_id}/reactions/{encoded_emoji}/@me"
    )
}
