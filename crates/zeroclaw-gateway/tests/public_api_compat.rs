#![cfg(feature = "gateway-voice-duplex")]

use zeroclaw_api::channel::{ChannelConversationScope, ChannelMessage};
use zeroclaw_gateway::voice_duplex::VoiceEvent;

fn classify_legacy_voice_event(event: VoiceEvent) -> &'static str {
    match event {
        VoiceEvent::SpeechStart => "speech_start",
        VoiceEvent::SpeechEnd { .. } => "speech_end",
        VoiceEvent::BargeIn => "barge_in",
        VoiceEvent::TtsCancel => "tts_cancel",
        VoiceEvent::TtsChunk { .. } => "tts_chunk",
    }
}

#[test]
fn pre_voicehost_gateway_api_still_compiles_for_downstream_users() {
    assert_eq!(classify_legacy_voice_event(VoiceEvent::BargeIn), "barge_in");

    let message = ChannelMessage {
        id: "message-id".into(),
        sender: "sender".into(),
        reply_target: "room".into(),
        content: "hello".into(),
        channel: "external-channel".into(),
        channel_alias: None,
        timestamp: 0,
        thread_ts: None,
        interruption_scope_id: None,
        attachments: Vec::new(),
        subject: None,
        internal_sop_event: None,
        passive_context: false,
        explicitly_addressed: false,
        conversation_scope: ChannelConversationScope::Sender,
        references: Vec::new(),
    };

    assert_eq!(message.content, "hello");
}
