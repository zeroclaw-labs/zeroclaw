//! Discord application-command interaction plumbing: the followup-credential
//! store, and the REST callbacks that ack (defer), refuse (reject), and answer
//! (edit @original) an interaction. The listen-loop dispatch arm and the
//! authorization gate (`interaction_gate`, coupled to the channel filters) stay
//! in `mod.rs` and call down into these.

use serde_json::json;

use super::types::{DISCORD_MAX_MESSAGE_LENGTH, DiscordOutgoing};

/// Credentials needed to answer a deferred interaction later: the followup
/// webhook is addressed by application id + interaction token.
#[derive(Clone)]
pub(crate) struct PendingInteraction {
    pub(crate) app_id: String,
    pub(crate) token: String,
    pub(crate) created: std::time::Instant,
}

/// Discord interaction followup tokens are valid for 15 minutes.
pub(crate) const INTERACTION_TOKEN_TTL: std::time::Duration =
    std::time::Duration::from_secs(15 * 60);

/// Acknowledge an interaction within Discord's 3-second window with a
/// type-5 "deferred channel message" (the "thinking…" state).
pub(crate) async fn discord_defer_interaction(
    client: &reqwest::Client,
    interaction_id: &str,
    interaction_token: &str,
) -> anyhow::Result<()> {
    let url = format!(
        "https://discord.com/api/v10/interactions/{interaction_id}/{interaction_token}/callback"
    );
    // type 5 = DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE
    let body = json!({ "type": 5 });
    // without_url: reqwest transport errors embed the full request URL,
    // which here contains the interaction token (a live credential).
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(reqwest::Error::without_url)?;
    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp.text().await.unwrap_or_default();
        anyhow::bail!("interaction defer failed ({status}): {err}");
    }
    Ok(())
}

/// Extract a string option (`d.data.options[name].value`) from an
/// APPLICATION_COMMAND interaction payload. Empty string when absent.
pub(crate) fn interaction_string_option(d: &serde_json::Value, name: &str) -> String {
    d.get("data")
        .and_then(|x| x.get("options"))
        .and_then(|o| o.as_array())
        .and_then(|opts| {
            opts.iter()
                .find(|o| o.get("name").and_then(|n| n.as_str()) == Some(name))
        })
        .and_then(|o| o.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Answer a refused interaction immediately with an ephemeral message
/// (type 4, flags 64 = only the invoker sees it). Without any callback the
/// invoker stares at "The application did not respond" for 3 seconds, which
/// reads as a bug rather than a policy decision.
pub(crate) async fn discord_reject_interaction(
    client: &reqwest::Client,
    interaction_id: &str,
    interaction_token: &str,
    message: &str,
) -> anyhow::Result<()> {
    let url = format!(
        "https://discord.com/api/v10/interactions/{interaction_id}/{interaction_token}/callback"
    );
    let body = json!({
        "type": 4,
        "data": {
            "content": message,
            "flags": 64
        }
    });
    // without_url: transport errors embed the token-bearing URL.
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(reqwest::Error::without_url)?;
    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp.text().await.unwrap_or_default();
        anyhow::bail!("interaction reject failed ({status}): {err}");
    }
    Ok(())
}

/// Deliver the agent's answer by editing the deferred interaction response
/// (`PATCH /webhooks/{app_id}/{token}/messages/@original`). The token is valid
/// for 15 minutes; no bot auth header is required for the followup webhook.
pub(crate) async fn discord_edit_interaction_response(
    client: &reqwest::Client,
    app_id: &str,
    interaction_token: &str,
    content: &str,
) -> anyhow::Result<()> {
    let url = format!(
        "https://discord.com/api/v10/webhooks/{app_id}/{interaction_token}/messages/@original"
    );
    let trimmed: String = content.chars().take(DISCORD_MAX_MESSAGE_LENGTH).collect();
    if trimmed.chars().count() < content.chars().count() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "content_chars": content.chars().count(),
                })),
            "interaction reply truncated to Discord's 2000-char limit (chunked followups are a planned follow-up)"
        );
    }
    // without_url: transport errors embed the token-bearing URL.
    let resp = client
        .patch(&url)
        .json(&DiscordOutgoing::text(trimmed).to_rest_json())
        .send()
        .await
        .map_err(reqwest::Error::without_url)?;
    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp.text().await.unwrap_or_default();
        anyhow::bail!("interaction followup edit failed ({status}): {err}");
    }
    Ok(())
}
