//! Discord application-command interaction plumbing: the followup-credential
//! store, and the REST callbacks that ack (defer), refuse (reject), and answer
//! (edit @original) an interaction. The listen-loop dispatch arm and the
//! authorization gate (`interaction_gate`, coupled to the channel filters) stay
//! in `mod.rs` and call down into these.

use serde_json::json;

use super::embed::DiscordEmbed;
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
/// (`PATCH {api_base}/webhooks/{app_id}/{token}/messages/@original`). The token
/// is valid for 15 minutes; no bot auth header is required for the followup
/// webhook. Renders any `[EMBED:…]` the agent emitted: `embeds` is attached
/// alongside the (2000-char-capped) content via the same `DiscordOutgoing`
/// envelope the normal send path uses. `api_base` is injectable for tests.
pub(crate) async fn discord_edit_interaction_response(
    client: &reqwest::Client,
    app_id: &str,
    interaction_token: &str,
    api_base: &str,
    content: &str,
    embeds: &[DiscordEmbed],
) -> anyhow::Result<()> {
    let url = format!("{api_base}/webhooks/{app_id}/{interaction_token}/messages/@original");
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
    let payload = DiscordOutgoing {
        content: Some(trimmed),
        embeds: embeds.to_vec(),
        ..Default::default()
    };
    // without_url: transport errors embed the token-bearing URL.
    let resp = client
        .patch(&url)
        .json(&payload.to_rest_json())
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

#[cfg(test)]
mod embed_reply_tests {
    use super::*;
    use wiremock::matchers::{body_json, body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn slash_reply_attaches_embeds_to_the_original_edit() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/webhooks/app/tok/messages/@original"))
            .and(body_partial_json(serde_json::json!({
                "content": "see below",
                "embeds": [{ "title": "Report" }]
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let embed = DiscordEmbed {
            title: Some("Report".to_string()),
            ..Default::default()
        };
        discord_edit_interaction_response(
            &client,
            "app",
            "tok",
            &server.uri(),
            "see below",
            std::slice::from_ref(&embed),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn content_only_slash_reply_omits_the_embeds_key() {
        // No embeds → the @original edit body stays byte-stable {"content": …}.
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/webhooks/app/tok/messages/@original"))
            .and(body_json(serde_json::json!({ "content": "hi" })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        discord_edit_interaction_response(&client, "app", "tok", &server.uri(), "hi", &[])
            .await
            .unwrap();
    }
}
