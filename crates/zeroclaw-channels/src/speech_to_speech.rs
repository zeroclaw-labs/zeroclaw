//! Speech-to-speech broker channel.
//!
//! Bridges a hosted bidirectional voice model (e.g. Gemini Live) into
//! ZeroClaw as a broker channel: audio in, transcript/audio out, with a
//! broker persona steering how the model mediates the call. This module
//! currently holds only the `Channel` skeleton — the audio seam and session
//! handle land in a later task.

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

use gemini_live::session::{Event, Session};
use gemini_live::types::{FunctionDecl, Model, SetupConfig};
use zeroclaw_api::channel::{Channel, ChannelConversationScope, ChannelMessage, SendMessage};
use zeroclaw_config::schema::{ModelKind, SpeechToSpeechConfig};

/// Build the Gemini Live `SetupConfig` for a broker session.
///
/// This is the security boundary between the hosted speech model and
/// ZeroClaw's tool surface: the model is a caller-facing broker, not an
/// agent with shell/file/MCP access. It is handed **exactly two**
/// functions — `consult_agent` (relay the caller's request into the real
/// agent and speak back its reply) and `end_session` (hang up) — and
/// nothing else. In particular, no `ScopedToolRegistry` tool is ever wired
/// into this setup; the broker cannot invoke shell, file, MCP, or any other
/// agent-side tool directly. `persona` is the fully-assembled broker system
/// prompt (scenario/persona text is the caller's responsibility, mirroring
/// `SetupConfig::system_instruction`'s contract).
pub fn build_broker_setup(cfg: &SpeechToSpeechConfig, persona: &str) -> SetupConfig {
    let model = match cfg.model_kind {
        ModelKind::NativeAudio => Model::NativeAudio,
        ModelKind::HalfCascade => Model::HalfCascade,
    };
    // BCP-47 language only applies to half-cascade; native-audio infers
    // language from the audio itself (mirrors `SetupConfig::language`'s
    // documented contract).
    let language = if matches!(cfg.model_kind, ModelKind::HalfCascade) {
        cfg.language.clone()
    } else {
        None
    };

    SetupConfig {
        model,
        voice: cfg.voice.clone().unwrap_or_default(),
        language,
        system_instruction: persona.to_string(),
        temperature: cfg.temperature.unwrap_or(0.8),
        functions: vec![
            FunctionDecl {
                name: "consult_agent".into(),
                description: "Relay the caller's request to the real agent and speak back \
                              its reply. Call this whenever the caller asks for something \
                              that requires the agent's knowledge or actions; you cannot \
                              satisfy it yourself."
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "prompt": {
                            "type": "string",
                            "description": "The caller's request, restated for the agent."
                        }
                    },
                    "required": ["prompt"]
                }),
            },
            FunctionDecl {
                name: "end_session".into(),
                description: "Call this exactly once, when the call is over and it is time \
                              to hang up."
                    .into(),
                parameters: serde_json::json!({ "type": "object", "properties": {} }),
            },
        ],
        // Warm resumption is a later task's concern (the session driver
        // owns the handle across reconnects); a fresh broker setup always
        // starts unresumed.
        resume_handle: None,
    }
}

/// The `setup.tools[].functionDeclarations[].name` list a built `SetupConfig`
/// would expose on the wire, in declaration order. A thin test/audit helper
/// around `gemini_live::wire::build_setup` — see
/// `setup_exposes_only_consult_and_end_session` for the invariant this
/// exists to prove.
pub fn broker_tool_names(setup: &SetupConfig) -> Vec<String> {
    let v = gemini_live::wire::build_setup(setup);
    v["setup"]["tools"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .flat_map(|t| {
            t["functionDeclarations"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|f| f["name"].as_str().map(String::from))
        .collect()
}

/// Speech-to-speech broker channel — bridges a hosted bidirectional voice
/// model into ZeroClaw. `send`/`listen` are minimal stubs for now; broker
/// session logic is filled in by a later task.
pub struct SpeechToSpeechChannel {
    /// The alias key under `[channels.speech_to_speech.<alias>]` this
    /// handle is bound to. Used for attribution.
    alias: String,
    /// Precomputed `name()` key (`speech_to_speech.<alias>`), joined once at
    /// construction so `name()` can return a borrowed `&str`.
    name: String,
    /// Reserved for the audio seam + session handle added in a later task.
    #[allow(dead_code)]
    config: SpeechToSpeechConfig,
}

impl SpeechToSpeechChannel {
    pub fn new(alias: impl Into<String>, config: SpeechToSpeechConfig) -> Self {
        let alias = alias.into();
        let name = format!("speech_to_speech.{alias}");
        Self {
            alias,
            name,
            config,
        }
    }

    /// Build the inbound `ChannelMessage` for a `consult_agent` tool call:
    /// the caller's `prompt`, attributed as coming from this broker session,
    /// scoped so history is isolated per-caller (mirrors how a phone call is
    /// its own conversation, not shared across every voice-broker session on
    /// this alias). The caller is talking directly to the broker, which just
    /// explicitly relayed the request — this always bypasses the
    /// reply-intent precheck other channels use to guess whether a mention
    /// was meant for the bot.
    fn consult_message(&self, prompt: &str) -> ChannelMessage {
        ChannelMessage {
            channel_alias: Some(self.alias.clone()),
            explicitly_addressed: true,
            conversation_scope: ChannelConversationScope::Sender,
            ..ChannelMessage::new(
                uuid::Uuid::new_v4().to_string(),
                format!("voice-broker:{}", self.alias),
                self.alias.clone(),
                prompt,
                "speech_to_speech",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            )
        }
    }

    /// The broker session event loop: drains `session.recv_event()` and, on
    /// every `consult_agent` tool call, relays the caller's request onto
    /// `tx` as an ordinary inbound `ChannelMessage` and immediately acks the
    /// call back to the model (fire-and-forget — the model just needs to
    /// know the relay was accepted so it can keep the caller company while
    /// the agent works; the actual reply comes back later via `send()`,
    /// wired up in a later task). All other events (transcript, audio,
    /// close, `end_session`) are ignored here; later tasks handle them.
    /// Returns once the session's event stream ends.
    pub async fn run_session(
        &self,
        mut session: Session,
        tx: mpsc::Sender<ChannelMessage>,
    ) -> Result<()> {
        while let Some(event) = session.recv_event().await {
            if let Event::ToolCall { name, id, args } = event {
                if name == "consult_agent" {
                    let prompt = args
                        .get("prompt")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let msg = self.consult_message(prompt);
                    if tx.send(msg).await.is_err() {
                        // Orchestrator gone; nothing left to relay into.
                        break;
                    }
                }
                // Ack unconditionally (including unknown tool names) so the
                // model's turn is never left hanging; the crate does not
                // special-case tool names either (mirrors `ToolCall`'s own
                // contract: "the caller decides its semantics").
                let _ = session.send_tool_response(&id).await;
            }
        }
        Ok(())
    }
}

impl ::zeroclaw_api::attribution::Attributable for SpeechToSpeechChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::VoiceBroker,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for SpeechToSpeechChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn send(&self, _message: &SendMessage) -> Result<()> {
        // Session/audio delivery lands in a later task.
        Ok(())
    }

    async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Broker session loop lands in a later task.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gemini_live::session::{ClientConfig, Reconnector, SessionError};
    use gemini_live::transport::{FakeTransport, TransportError};
    use zeroclaw_api::channel::Channel;

    fn cfg() -> SpeechToSpeechConfig {
        SpeechToSpeechConfig::default()
    }

    /// Map a `SpeechToSpeechConfig` onto the `gemini-live` `ClientConfig`
    /// this test drives `Session::connect_with_transport` with. Production
    /// wiring (real `api_key`/`proxy`, reconnect budget) lands with the
    /// `listen()` integration in a later task; this is test-only plumbing.
    fn client_config(cfg: &SpeechToSpeechConfig) -> ClientConfig {
        let model = match cfg.model_kind {
            ModelKind::NativeAudio => Model::NativeAudio,
            ModelKind::HalfCascade => Model::HalfCascade,
        };
        ClientConfig {
            model,
            api_key: cfg.api_key.clone().unwrap_or_default(),
            proxy: None,
            setup: build_broker_setup(cfg, "you are a broker"),
            max_reconnect_attempts: None,
        }
    }

    /// A reconnector that always fails — the tests here never need a real
    /// reconnect; `FakeTransport::new(true)` keeps the session open past its
    /// scripted frames instead.
    fn no_reconnect() -> Reconnector<FakeTransport> {
        Box::new(|| {
            Box::pin(async {
                Err(SessionError::Transport(TransportError::Connect(
                    "no reconnect".into(),
                )))
            })
        })
    }

    #[tokio::test]
    async fn consult_agent_toolcall_emits_channel_message() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut fake = FakeTransport::new(true);
        fake.push_data(br#"{"setupComplete":{}}"#.to_vec());
        fake.push_data(
            br#"{"toolCall":{"functionCalls":[{"name":"consult_agent","id":"c1","args":{"prompt":"what's on my calendar?"}}]}}"#
                .to_vec(),
        );
        let session = Session::connect_with_transport(client_config(&cfg()), fake, no_reconnect())
            .await
            .unwrap();
        let ch = SpeechToSpeechChannel::new("desk".to_string(), cfg());
        tokio::spawn(async move {
            let _ = ch.run_session(session, tx).await;
        });

        let msg = rx.recv().await.expect("a channel message");
        assert_eq!(msg.content, "what's on my calendar?");
        assert_eq!(msg.channel_alias.as_deref(), Some("desk"));
        assert!(matches!(
            msg.conversation_scope,
            ChannelConversationScope::Sender
        ));
    }

    #[test]
    fn channel_name_is_alias_scoped() {
        let ch = SpeechToSpeechChannel::new("desk".to_string(), cfg());
        assert_eq!(ch.name(), "speech_to_speech.desk");
    }

    #[test]
    fn setup_exposes_only_consult_and_end_session() {
        let setup = build_broker_setup(&cfg(), "you are a broker");
        let v = gemini_live::wire::build_setup(&setup);
        let tools: Vec<String> = v["setup"]["tools"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .flat_map(|t| {
                t["functionDeclarations"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .filter_map(|f| f["name"].as_str().map(String::from))
            .collect();
        let mut sorted = tools.clone();
        sorted.sort();
        assert_eq!(
            sorted,
            vec!["consult_agent".to_string(), "end_session".to_string()],
            "broker setup must expose ONLY consult_agent + end_session, got {tools:?}"
        );
    }

    #[test]
    fn setup_emits_compression_resumption_and_transcription() {
        let v = gemini_live::wire::build_setup(&build_broker_setup(&cfg(), "p"));
        assert!(v["setup"]["contextWindowCompression"]["slidingWindow"].is_object());
        assert!(v["setup"]["sessionResumption"].is_object());
        assert!(v["setup"]["inputAudioTranscription"].is_object());
        assert!(v["setup"]["outputAudioTranscription"].is_object());
    }
}
