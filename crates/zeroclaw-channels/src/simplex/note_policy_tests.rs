use super::fixture;
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};
use zeroclaw_config::{
    live::LiveConfig,
    schema::{OpenAITtsProviderConfig, TtsProviderConfig},
};

const FIRST_PART: &str =
    "The first sentence was already synthesized while the response was still being generated.";
const NEXT_PART: &str = "This queued sentence must never reach the superseded provider.";

#[derive(Clone, Copy, Debug)]
enum LivePolicyChange {
    RevokePeer,
    DisableTts,
    SwitchProvider,
}

fn openai_tts(endpoint: &str) -> OpenAITtsProviderConfig {
    OpenAITtsProviderConfig {
        base: TtsProviderConfig {
            api_key: Some("fixture".into()),
            uri: Some(format!("{endpoint}/speech")),
            response_format: Some("wav".into()),
            ..Default::default()
        },
    }
}

fn apply_live_policy_change(change: LivePolicyChange, live: &LiveConfig) {
    let mut config = live.snapshot();
    match change {
        LivePolicyChange::RevokePeer => {
            config
                .peer_groups
                .get_mut("owner")
                .unwrap()
                .external_peers
                .retain(|peer| zeroclaw_config::schema::peer_grant_identity(peer) != Some("17"));
        }
        LivePolicyChange::DisableTts => config.tts.enabled = false,
        LivePolicyChange::SwitchProvider => {
            config.agents.get_mut("assistant").unwrap().tts_provider = "openai.replacement".into();
            config
                .providers
                .tts
                .openai
                .insert("replacement".into(), openai_tts("http://127.0.0.1:9"));
        }
    }
    live.publish(live.next_revision().unwrap(), config).unwrap();
}

async fn assert_queued_note_stops_using_superseded_provider(change: LivePolicyChange) {
    let (channel, live) = fixture();
    let handler_live = live.clone();
    let old_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/speech"))
        .respond_with(move |_: &Request| {
            // Both segments are queued. Publish before the worker can receive
            // the first response and start the second provider request.
            apply_live_policy_change(change, &handler_live);
            ResponseTemplate::new(200).set_body_bytes(b"fixture audio".to_vec())
        })
        .mount(&old_server)
        .await;

    let mut config = live.snapshot();
    config.tts.enabled = true;
    config.agents.get_mut("assistant").unwrap().tts_provider = "openai.old".into();
    config
        .providers
        .tts
        .openai
        .insert("old".into(), openai_tts(&old_server.uri()));
    live.publish(live.next_revision().unwrap(), config).unwrap();
    let note = channel.start_note("17", true).unwrap();
    let final_text = format!("{FIRST_PART}\n\n{NEXT_PART}");
    let error = note.job.finish(&final_text).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "simplex_voice_policy_changed",
        "{change:?} must invalidate the queued note"
    );

    let old_requests = old_server.received_requests().await.unwrap();
    assert_eq!(
        old_requests.len(),
        1,
        "{change:?} must not send {NEXT_PART:?} to the old provider"
    );
    let first_body: serde_json::Value = serde_json::from_slice(&old_requests[0].body).unwrap();
    assert_eq!(first_body["input"], FIRST_PART);
}

#[tokio::test]
async fn queued_note_never_uses_its_old_provider_after_live_policy_changes() {
    for change in [
        LivePolicyChange::RevokePeer,
        LivePolicyChange::DisableTts,
        LivePolicyChange::SwitchProvider,
    ] {
        assert_queued_note_stops_using_superseded_provider(change).await;
    }
}
