//! Discord application-command interaction plumbing: the followup-credential
//! store, and the REST callbacks that ack (defer), refuse (reject), and answer
//! (edit @original) an interaction. The listen-loop dispatch arm and the
//! authorization gate (`interaction_gate`, coupled to the channel filters) stay
//! in `mod.rs` and call down into these.

use serde_json::json;

use super::embed::DiscordEmbed;
use super::types::DiscordOutgoing;

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

/// Open a modal in response to a button/slash interaction (callback type 9).
/// The caller must register the modal's `custom_id` in the pending registry
/// first so the eventual type-5 submit can resolve it. Sender-side — the first
/// caller lands with a feature that opens a modal (EPIC B Phase 4 onward).
#[allow(dead_code)]
pub(crate) async fn discord_open_modal(
    client: &reqwest::Client,
    interaction_id: &str,
    interaction_token: &str,
    modal: &super::components::DiscordModal,
) -> anyhow::Result<()> {
    let Some(data) = modal.to_api() else {
        anyhow::bail!("modal custom_id exceeds Discord's 100-char limit; cannot open");
    };
    let url = format!(
        "https://discord.com/api/v10/interactions/{interaction_id}/{interaction_token}/callback"
    );
    // type 9 = MODAL
    let body = json!({ "type": 9, "data": data });
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
        anyhow::bail!("modal open failed ({status}): {err}");
    }
    Ok(())
}

/// Answer an APPLICATION_COMMAND_AUTOCOMPLETE (type 4) interaction with a
/// callback type 8 (APPLICATION_COMMAND_AUTOCOMPLETE_RESULT) carrying inline
/// choices. Discord shows up to 25 choices; over that it 400s, so the caller's
/// list is truncated here. An empty list is a valid answer (clears the
/// suggestion box) — used when authz fails or no choices are available, so a
/// per-keystroke event never hangs or leaks a policy decision.
///
/// Unlike `discord_reject_interaction`/`discord_defer_interaction`, this is the
/// ONLY response an autocomplete keystroke may produce: it neither defers nor
/// posts an ephemeral, so authorization stays side-effect-free.
pub(crate) async fn discord_answer_autocomplete(
    client: &reqwest::Client,
    interaction_id: &str,
    interaction_token: &str,
    choices: &[(String, String)],
) -> anyhow::Result<()> {
    let url = format!(
        "https://discord.com/api/v10/interactions/{interaction_id}/{interaction_token}/callback"
    );
    let rendered: Vec<serde_json::Value> = choices
        .iter()
        .take(25)
        .map(|(name, value)| json!({ "name": name, "value": value }))
        .collect();
    // type 8 = APPLICATION_COMMAND_AUTOCOMPLETE_RESULT
    let body = json!({ "type": 8, "data": { "choices": rendered } });
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
        anyhow::bail!("interaction autocomplete answer failed ({status}): {err}");
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

/// Edit the deferred interaction's @original message
/// (`PATCH {api_base}/webhooks/{app_id}/{token}/messages/@original`). The token
/// is valid for 15 minutes; no bot auth header is required for the followup
/// webhook. Renders any `[EMBED:…]` the agent emitted by attaching `embeds`
/// alongside the content via the same `DiscordOutgoing` envelope the normal send
/// path uses. `content` must be within Discord's 2000-char limit — callers whose
/// reply may exceed it chunk first and post the remainder via
/// [`discord_post_interaction_followup`]. `api_base` is injectable for tests.
pub(crate) async fn discord_edit_interaction_response(
    client: &reqwest::Client,
    app_id: &str,
    interaction_token: &str,
    api_base: &str,
    content: &str,
    embeds: &[DiscordEmbed],
) -> anyhow::Result<()> {
    let url = format!("{api_base}/webhooks/{app_id}/{interaction_token}/messages/@original");
    // No truncation: the caller chunks (deliver_interaction_answer) and this edit
    // carries the first ≤2000-char chunk plus any embeds.
    let payload = DiscordOutgoing {
        content: Some(content.to_string()),
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

/// Post an additional interaction followup message
/// (`POST {api_base}/webhooks/{app_id}/{token}`), used to deliver the answer
/// chunks beyond the first when a reply exceeds Discord's 2000-char limit.
pub(crate) async fn discord_post_interaction_followup(
    client: &reqwest::Client,
    app_id: &str,
    interaction_token: &str,
    api_base: &str,
    content: &str,
) -> anyhow::Result<()> {
    let url = format!("{api_base}/webhooks/{app_id}/{interaction_token}");
    let resp = client
        .post(&url)
        .json(&DiscordOutgoing::text(content).to_rest_json())
        .send()
        .await
        .map_err(reqwest::Error::without_url)?;
    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp.text().await.unwrap_or_default();
        anyhow::bail!("interaction followup post failed ({status}): {err}");
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
