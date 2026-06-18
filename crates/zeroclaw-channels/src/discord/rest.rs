//! Outbound Discord REST: turn a message (content + future embeds/components,
//! via [`DiscordOutgoing`]) plus optional files into an HTTP request and parse
//! the response. Channel-message + reaction-URL builders only — interaction
//! callbacks (defer/reject/followup) live in `interaction`.

use std::fmt::Write as _;
use std::path::PathBuf;

use reqwest::multipart::{Form, Part};

use super::types::DiscordOutgoing;

/// POST a plain-text message and return the new message's ID. Callers
/// that don't need the ID (e.g. non-first chunks) can discard it.
pub(crate) async fn send_discord_message_json(
    client: &reqwest::Client,
    bot_token: &str,
    recipient: &str,
    content: &str,
) -> anyhow::Result<String> {
    let url = format!("https://discord.com/api/v10/channels/{recipient}/messages");
    let body = DiscordOutgoing::text(content).to_rest_json();

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

/// POST a message with file attachments via multipart, returning the new
/// message's ID. Callers that don't need the ID can discard it.
pub(crate) async fn send_discord_message_with_files(
    client: &reqwest::Client,
    bot_token: &str,
    recipient: &str,
    content: &str,
    files: &[PathBuf],
) -> anyhow::Result<String> {
    let url = format!("https://discord.com/api/v10/channels/{recipient}/messages");

    let mut form = Form::new().text(
        "payload_json",
        DiscordOutgoing::text(content).payload_json(),
    );

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

/// Edit an existing Discord message via PATCH.
///
/// Returns `Ok(())` on success. On HTTP 429 (rate limited), logs at debug
/// level and returns `Ok(())` since skipping a mid-stream edit is harmless.
pub(crate) async fn edit_discord_message(
    client: &reqwest::Client,
    bot_token: &str,
    channel_id: &str,
    message_id: &str,
    content: &str,
) -> anyhow::Result<()> {
    let url = format!("https://discord.com/api/v10/channels/{channel_id}/messages/{message_id}");
    let body = DiscordOutgoing::text(content).to_rest_json();

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
///
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

/// URL-encode a Unicode emoji for use in Discord reaction API paths.
///
/// Discord's reaction endpoints accept raw Unicode emoji in the URL path,
/// but they must be percent-encoded per RFC 3986. Custom guild emojis use
/// the `name:id` format and are passed through unencoded.
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
