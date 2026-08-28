//! Speech-to-speech broker channel.
//!
//! Bridges a hosted bidirectional voice model (e.g. Gemini Live) into
//! ZeroClaw as a broker channel: audio in, transcript/audio out, with a
//! broker persona steering how the model mediates the call. This module
//! currently holds only the `Channel` skeleton — the audio seam and session
//! handle land in a later task.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::sync::{Notify, mpsc};

use gemini_live::session::{ClientTextSender, Event, Session, SessionError};
use gemini_live::types::{FunctionDecl, Model, SetupConfig};
use zeroclaw_api::channel::{Channel, ChannelConversationScope, ChannelMessage, SendMessage};
use zeroclaw_config::paths::{normalize_lexical, resolve_under};
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
        voice: cfg
            .voice
            .clone()
            .unwrap_or_else(|| default_voice(cfg.model_kind).to_string()),
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

/// The per-`model_kind` default voice used when `cfg.voice` is unset —
/// spec §3: `Autonoe` for native-audio, a Kore-class voice for
/// half-cascade.
fn default_voice(model_kind: ModelKind) -> &'static str {
    match model_kind {
        ModelKind::NativeAudio => "Autonoe",
        ModelKind::HalfCascade => "Kore",
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

/// How long [`SpeechToSpeechChannel::run_session`] waits for a new session
/// event before treating the call as abandoned and closing it. Reset every
/// time an event is received (see `run_session`'s `select!` loop).
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Fallback broker system-instruction used when neither
/// `broker_persona_path` nor `broker_persona` is configured. Deliberately
/// generic and standalone: this is the voice-facing broker's persona, not
/// the agent's own instructions, so it must never fall back to (or even
/// mention) `AGENTS.md`.
const DEFAULT_BROKER_PERSONA: &str = "You are a helpful voice call broker. Speak naturally \
    and concisely, as if on a phone call. When the caller asks for something you cannot \
    answer yourself, relay it to the agent via consult_agent and relay its reply back in \
    your own words. When the call is over, end it with end_session.";

/// Resolve `raw_path` — interpreted, per `broker_persona_path`'s documented
/// contract, as relative to the workspace — against `workspace_dir`, and
/// reject anything that would escape it: an absolute path outside the
/// workspace, or `..` traversal. A symlink whose canonical target stays
/// inside the workspace is accepted (workspace-internal aliases are
/// legitimate); one that lands outside is rejected. Mirrors
/// `wechat::WeChatChannel::resolve_local_attachment_path`'s containment
/// logic (see `canonicalize_within_workspace` there) built from the same
/// `zeroclaw_config::paths` primitives — reimplemented here rather than
/// shared because this module has no `WeChatChannel` to borrow it from.
fn resolve_persona_path_within_workspace(raw_path: &str, workspace_dir: &Path) -> Result<PathBuf> {
    let candidate = Path::new(raw_path);
    let normalized = if candidate.is_absolute() {
        let normalized = normalize_lexical(candidate);
        let workspace_normalized = normalize_lexical(workspace_dir);
        if !normalized.starts_with(&workspace_normalized) {
            anyhow::bail!(
                "broker_persona_path {raw_path:?} escapes workspace {}",
                workspace_dir.display()
            );
        }
        normalized
    } else {
        resolve_under(workspace_dir, raw_path).map_err(|e| {
            anyhow::Error::msg(format!(
                "broker_persona_path {raw_path:?} escapes workspace {}: {e}",
                workspace_dir.display()
            ))
        })?
    };

    // Canonicalize to also catch a symlink whose target lands outside the
    // workspace. A not-yet-existing path can't be canonicalized; skip the
    // check there (mirrors wechat's behavior) — `read_to_string` will fail
    // on it below with a clear "not found" error instead.
    match std::fs::canonicalize(&normalized) {
        Ok(canonical) => {
            let workspace_canonical = std::fs::canonicalize(workspace_dir).with_context(|| {
                format!(
                    "workspace_dir {} could not be canonicalized",
                    workspace_dir.display()
                )
            })?;
            if !canonical.starts_with(&workspace_canonical) {
                anyhow::bail!(
                    "broker_persona_path {raw_path:?} canonicalizes to {} which escapes \
                     workspace {}",
                    canonical.display(),
                    workspace_canonical.display()
                );
            }
            Ok(canonical)
        }
        Err(_) => Ok(normalized),
    }
}

/// Resolve the broker persona system-instruction for a session, standalone
/// from (and never reading) `AGENTS.md` or any agent-side prompt material.
/// Precedence: `broker_persona_path` (file contents, read fresh every call)
/// wins over inline `broker_persona`, which wins over
/// [`DEFAULT_BROKER_PERSONA`]. A configured path that fails to read, or that
/// resolves outside `workspace_dir`, is a real configuration error and
/// propagates as `Err` rather than silently falling back to the default.
///
/// **Security:** `broker_persona_path` is confined to `workspace_dir` (see
/// [`resolve_persona_path_within_workspace`]) — an absolute path outside the
/// workspace or a `..`-escaping relative path is rejected rather than read,
/// since its contents are shipped to the provider as the session's system
/// instruction.
pub fn resolve_persona(cfg: &SpeechToSpeechConfig, workspace_dir: &Path) -> Result<String> {
    if let Some(path) = &cfg.broker_persona_path {
        let resolved = resolve_persona_path_within_workspace(path, workspace_dir)?;
        return std::fs::read_to_string(&resolved).map_err(|e| {
            anyhow::Error::msg(format!("failed to read broker_persona_path {path:?}: {e}"))
        });
    }
    if let Some(persona) = &cfg.broker_persona {
        return Ok(persona.clone());
    }
    Ok(DEFAULT_BROKER_PERSONA.to_string())
}

/// Speech-to-speech broker channel — bridges a hosted bidirectional voice
/// model into ZeroClaw. `send`/`listen` are minimal stubs for now; broker
/// session logic is filled in by a later task.
pub struct SpeechToSpeechChannel {
    /// The alias key under `[channels.speech_to_speech.<alias>]` this
    /// handle is bound to. Used for attribution, history scoping, and by
    /// the orchestrator to build the `speech_to_speech.<alias>` composite
    /// registry key (`Channel::name()` returns only the bare type name —
    /// see its doc comment).
    alias: String,
    /// The text-send handle for whichever session is currently live on this
    /// alias, if any. Populated by [`Self::attach_session`] (called by
    /// [`Self::run_session`] for the session it drives), consumed by
    /// [`Channel::send`] to relay the agent's settled reply back in as a
    /// paraphrase turn.
    ///
    /// v1 assumption: **single active session per alias.** A second
    /// `attach_session` call (e.g. a reconnect, or — not yet possible today —
    /// a second concurrent call on the same alias) simply overwrites the
    /// handle; there is no queuing or fan-out across multiple simultaneous
    /// sessions. `Mutex` (not `tokio::sync::Mutex`) because the critical
    /// section is a plain pointer swap/clone, never held across an `.await`.
    active_session: Arc<Mutex<Option<ClientTextSender>>>,
    /// Authoritative manual-close signal for [`Self::run_session`]:
    /// [`Self::stop`] notifies this, and the loop's `select!` treats it as
    /// an unconditional break — it wins even mid-model-turn, ahead of
    /// whatever the model is doing. `Arc<Notify>` (not a `watch`) because
    /// there is exactly one thing to communicate ("close now") and no state
    /// to observe after the fact.
    stop: Arc<Notify>,
    /// How long `run_session` waits for a new session event before treating
    /// the call as abandoned and closing it as an idle-timeout backstop.
    /// Defaults to [`DEFAULT_IDLE_TIMEOUT`]; overridden only by tests via
    /// [`Self::with_idle_timeout`] so they can force the branch quickly.
    idle_timeout: Duration,
}

impl SpeechToSpeechChannel {
    pub fn new(alias: impl Into<String>) -> Self {
        let alias = alias.into();
        Self {
            alias,
            active_session: Arc::new(Mutex::new(None)),
            stop: Arc::new(Notify::new()),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
        }
    }

    /// Test-only knob: override the idle timeout so a test can force the
    /// idle-timeout close path quickly instead of waiting out the real
    /// [`DEFAULT_IDLE_TIMEOUT`]. Production always uses the default set in
    /// [`Self::new`].
    #[cfg(test)]
    fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout;
        self
    }

    /// Authoritatively end whichever session [`Self::run_session`] is
    /// currently driving on this alias, even mid-model-turn. Takes priority
    /// over any in-flight event in the `select!` loop — this is a hard
    /// close, not a request the model can defer.
    pub async fn stop(&self) {
        self.stop.notify_one();
    }

    /// Record `session` as this alias's active session: `send()` will relay
    /// into it until a later `attach_session` call (or `run_session` ending)
    /// replaces/clears it. Only clones the session's cloneable
    /// [`ClientTextSender`] — never takes ownership of `session` itself, so
    /// the caller keeps it (typically to drive [`Self::run_session`]
    /// immediately afterward).
    pub(crate) fn attach_session(&self, session: &Session) {
        *self.active_session.lock().unwrap() = Some(session.text_sender());
    }

    /// Build a `ChannelMessage` carrying this alias's history-scope
    /// identity fields: `channel` = `"speech_to_speech"`, `channel_alias` =
    /// this alias, `sender` = `"voice-broker:{alias}"`, `reply_target` =
    /// this alias, `conversation_scope` = `Sender`, no `thread_ts`. `id` and
    /// `content` are the only fields callers vary — neither feeds
    /// `orchestrator::conversation_history_key`, so any two messages built
    /// here for the same alias resolve to the same history bucket. This is
    /// the single source of truth for that identity: [`Self::consult_message`]
    /// and [`Self::history_key_for_transcript`] both build through it,
    /// which is what guarantees a future transcript handler and the consult
    /// turn land in the same conversation history (see
    /// [`Self::history_key_for_sender`]).
    fn scoped_message(&self, id: impl Into<String>, content: impl Into<String>) -> ChannelMessage {
        ChannelMessage {
            channel_alias: Some(self.alias.clone()),
            explicitly_addressed: true,
            conversation_scope: ChannelConversationScope::Sender,
            ..ChannelMessage::new(
                id,
                format!("voice-broker:{}", self.alias),
                self.alias.clone(),
                content,
                "speech_to_speech",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            )
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
        self.scoped_message(uuid::Uuid::new_v4().to_string(), prompt)
    }

    /// The conversation-history key this alias's consult turns resolve to —
    /// `orchestrator::conversation_history_key` applied to
    /// [`Self::scoped_message`]'s identity fields, the same fields
    /// [`Self::consult_message`] builds its `ChannelMessage` from. This is
    /// the shared-key contract PR1 establishes: whatever routes a message
    /// through `scoped_message` for this alias — the consult turn today, a
    /// transcript handler in a later PR — lands in this same history
    /// bucket. See [`Self::history_key_for_transcript`] for the transcript
    /// side of that guarantee, proven equal in
    /// `transcript_binds_to_consult_history_key`.
    #[cfg(test)]
    fn history_key_for_sender(&self) -> String {
        crate::orchestrator::conversation_history_key(&self.scoped_message("_", ""))
    }

    /// The conversation-history key a broker transcript for this alias
    /// would resolve to, computed the same way
    /// [`Self::history_key_for_sender`] is: through
    /// [`Self::scoped_message`]'s shared identity fields. PR1 scope: no
    /// transcript is actually routed into any history store yet (`Event::
    /// Transcript` handling and the store itself land with the audio-WS
    /// PR) — this exists only to prove the derived key is identical to the
    /// consult turn's, so that later wiring is pure plumbing, not a scoping
    /// decision.
    #[cfg(test)]
    fn history_key_for_transcript(&self) -> String {
        crate::orchestrator::conversation_history_key(
            &self.scoped_message(uuid::Uuid::new_v4().to_string(), ""),
        )
    }

    /// The broker session event loop: drains `session.recv_event()` and, on
    /// every `consult_agent` tool call, relays the caller's request onto
    /// `tx` as an ordinary inbound `ChannelMessage` and immediately acks the
    /// call back to the model (fire-and-forget — the model just needs to
    /// know the relay was accepted so it can keep the caller company while
    /// the agent works; the actual reply comes back later via `send()`,
    /// wired up in a later task). Other events (transcript, audio, etc.) are
    /// otherwise ignored — they only serve to reset the idle timer below.
    ///
    /// Three paths close the session, raced via `select!`:
    ///   1. **Model-initiated:** `Event::ToolCall{name:"end_session"}` — ack
    ///      it (so the model's turn is never left hanging), then break.
    ///   2. **Manual stop (authoritative):** [`Self::stop`] notifies
    ///      `self.stop`; this wins even mid-model-turn, `biased` ahead of
    ///      the other arms.
    ///   3. **Idle-timeout backstop:** no session event within
    ///      `self.idle_timeout`; the sleep is rebuilt (and so reset) every
    ///      loop iteration, i.e. on every received event.
    ///
    /// The provider ending the stream itself (`Event::SessionClosed`, or
    /// `recv_event` returning `None` once reconnects are exhausted) also
    /// ends the loop gracefully. On any break, the active-session handle is
    /// cleared so a stale `send()` cannot relay into a dead session.
    pub async fn run_session(
        &self,
        mut session: Session,
        tx: mpsc::Sender<ChannelMessage>,
    ) -> Result<()> {
        self.attach_session(&session);
        loop {
            let idle = tokio::time::sleep(self.idle_timeout);
            tokio::select! {
                biased;

                // Manual stop is authoritative: check it first so a
                // simultaneously-ready event never gets processed ahead of
                // it.
                _ = self.stop.notified() => {
                    break;
                }

                event = session.recv_event() => {
                    match event {
                        Some(Event::ToolCall { name, id, args }) => {
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
                            // Ack unconditionally (including unknown tool
                            // names) so the model's turn is never left
                            // hanging; the crate does not special-case tool
                            // names either (mirrors `ToolCall`'s own
                            // contract: "the caller decides its semantics").
                            let _ = session.send_tool_response(&id).await;
                            if name == "end_session" {
                                break;
                            }
                        }
                        Some(Event::SessionClosed { .. }) | None => {
                            // Provider-initiated close (terminal, or
                            // reconnects exhausted): nothing left to drive.
                            break;
                        }
                        Some(_other) => {
                            // Transcript/audio/affect/etc: no action here;
                            // looping restarts the idle timer above.
                        }
                    }
                }

                _ = idle => {
                    // No session activity within idle_timeout; treat the
                    // call as abandoned.
                    break;
                }
            }
        }
        // Per the single-active-session assumption (see `active_session`'s
        // doc) this alias never has two sessions attached at once, so an
        // unconditional clear is safe on every break path above.
        *self.active_session.lock().unwrap() = None;
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
    /// Bare type name — matches every other `Channel` impl's convention
    /// (`"telegram"`, `"voice_call"`, ...). The orchestrator's
    /// `composite_channel_key(name(), alias())` builds the
    /// `speech_to_speech.<alias>` registry key from this plus [`Self::alias`];
    /// `name()` itself must stay alias-free, since `append_configured_plugin_channels`
    /// and other callers rely on `name()` identifying the channel *type*.
    fn name(&self) -> &str {
        "speech_to_speech"
    }

    /// Relay the agent's settled reply into the live broker session as a
    /// paraphrase text turn (`send_client_text`) — the caller hears it
    /// spoken back by the broker persona, not verbatim TTS of raw agent
    /// output. Per the single-active-session assumption (see
    /// `active_session`'s doc), there is no correlation to a specific call:
    /// whichever session is currently attached on this alias receives it.
    /// If no session is active (call already ended, or none ever started),
    /// this logs and returns `Ok(())` rather than erroring — a reply arriving
    /// after hangup is a race, not a failure. The same treatment applies if
    /// a session *was* attached but died between attach and this call
    /// (`SessionError::Closed`): only genuinely-unexpected relay failures
    /// surface as `Err`.
    async fn send(&self, message: &SendMessage) -> Result<()> {
        let sender = self.active_session.lock().unwrap().clone();
        match sender {
            Some(sender) => match sender.send_client_text(&message.content).await {
                Ok(()) => Ok(()),
                Err(SessionError::Closed) => {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"alias": self.alias})),
                        "speech_to_speech: send() raced session teardown; dropping reply"
                    );
                    Ok(())
                }
                Err(e) => Err(anyhow::Error::msg(format!(
                    "speech_to_speech.{}: failed to relay reply into live session: {e}",
                    self.alias
                ))),
            },
            None => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"alias": self.alias})),
                    "speech_to_speech: send() with no active session; dropping reply"
                );
                Ok(())
            }
        }
    }

    async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Broker session loop lands in a later task.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Tests spawn detached driver tasks directly and do not need the
    // attribution-span propagation `zeroclaw_spawn::spawn!` provides in
    // production, so `tokio::spawn` is allowed in this test module
    // (clippy.toml sanctions a local allow for exempt cases).
    #![allow(clippy::disallowed_methods)]

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
    async fn send_relays_reply_into_live_session_as_text() {
        // `FakeTransport::sent` (an `Arc<Mutex<Vec<String>>>`, already public
        // on the crate's test double) already records every outbound frame
        // byte-for-byte — no need for a dedicated recorder: clone the handle
        // before the transport moves into `connect_with_transport` and
        // inspect it directly, the same way `gemini_live::session`'s own
        // tests do (see `send_audio_is_byte_identical_to_kutsu`).
        let fake = FakeTransport::new(true);
        let sent = fake.sent.clone();
        let session = Session::connect_with_transport(client_config(&cfg()), fake, no_reconnect())
            .await
            .unwrap();
        let ch = SpeechToSpeechChannel::new("desk".to_string());
        ch.attach_session(&session);

        ch.send(&SendMessage::new(
            "You have a 3pm sync.",
            "voice-broker:desk",
        ))
        .await
        .unwrap();

        // The driver task performs the transport write asynchronously after
        // the command is enqueued; poll briefly rather than assuming
        // immediacy (mirrors `wait_for_sent` in `gemini_live::session`'s own
        // tests).
        let mut found = false;
        for _ in 0..1000 {
            if sent.lock().unwrap().iter().any(|t| t.contains("3pm sync")) {
                found = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert!(
            found,
            "reply must be relayed via send_client_text, got {:?}",
            sent.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn send_with_no_active_session_is_a_noop() {
        let ch = SpeechToSpeechChannel::new("desk".to_string());
        ch.send(&SendMessage::new("hello", "voice-broker:desk"))
            .await
            .expect("send() with no active session must not error");
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
        let ch = SpeechToSpeechChannel::new("desk".to_string());
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

    #[tokio::test]
    async fn model_end_session_closes() {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let mut fake = FakeTransport::new(true);
        fake.push_data(br#"{"setupComplete":{}}"#.to_vec());
        fake.push_data(
            br#"{"toolCall":{"functionCalls":[{"name":"end_session","id":"e1","args":{}}]}}"#
                .to_vec(),
        );
        let sent = fake.sent.clone();
        let session = Session::connect_with_transport(client_config(&cfg()), fake, no_reconnect())
            .await
            .unwrap();
        let ch = SpeechToSpeechChannel::new("desk".to_string());

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            ch.run_session(session, tx),
        )
        .await
        .expect("run_session must return promptly once the model calls end_session")
        .unwrap();

        assert!(
            ch.active_session.lock().unwrap().is_none(),
            "session must be detached once run_session closes it"
        );

        // The ack write happens on the driver task, asynchronously from the
        // enqueue in `run_session` (mirrors `send_relays_reply_into_live_
        // session_as_text`'s poll below) — the mpsc channel drains already-
        // queued commands even after `Session` (and so `cmd_tx`) is dropped
        // at the end of `run_session`, but that drain isn't necessarily
        // done by the time this assertion runs.
        let mut acked = false;
        for _ in 0..1000 {
            if sent.lock().unwrap().iter().any(|t| t.contains("e1")) {
                acked = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert!(
            acked,
            "end_session call must still be acked before closing, got {:?}",
            sent.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn manual_stop_is_authoritative() {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let mut fake = FakeTransport::new(true);
        fake.push_data(br#"{"setupComplete":{}}"#.to_vec());
        let session = Session::connect_with_transport(client_config(&cfg()), fake, no_reconnect())
            .await
            .unwrap();
        let ch = Arc::new(SpeechToSpeechChannel::new("desk".to_string()));
        let driver = {
            let ch = ch.clone();
            tokio::spawn(async move { ch.run_session(session, tx).await })
        };

        // Give run_session a moment to attach the session and start waiting
        // on the select! (mid-turn, no end_session and no idle-timeout in
        // sight) before we call the authoritative stop.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        ch.stop().await;

        tokio::time::timeout(std::time::Duration::from_secs(2), driver)
            .await
            .expect("run_session must return promptly once stop() is called")
            .unwrap()
            .unwrap();

        assert!(
            ch.active_session.lock().unwrap().is_none(),
            "session must be detached once stop() closes it"
        );
    }

    #[tokio::test]
    async fn idle_timeout_closes_session() {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let mut fake = FakeTransport::new(true);
        fake.push_data(br#"{"setupComplete":{}}"#.to_vec());
        let session = Session::connect_with_transport(client_config(&cfg()), fake, no_reconnect())
            .await
            .unwrap();
        let ch = SpeechToSpeechChannel::new("desk".to_string())
            .with_idle_timeout(std::time::Duration::from_millis(50));

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            ch.run_session(session, tx),
        )
        .await
        .expect("run_session must return once the idle timeout elapses")
        .unwrap();

        assert!(
            ch.active_session.lock().unwrap().is_none(),
            "session must be detached once the idle timeout closes it"
        );
    }

    #[test]
    fn transcript_binds_to_consult_history_key() {
        let ch = SpeechToSpeechChannel::new("desk".to_string());
        let consult_key = ch.history_key_for_sender(); // used by consult_message
        let transcript_key = ch.history_key_for_transcript();
        assert_eq!(consult_key, transcript_key);
    }

    #[test]
    fn channel_name_is_bare_type_alias_resolves_composite() {
        use ::zeroclaw_api::attribution::Attributable;

        let ch = SpeechToSpeechChannel::new("desk".to_string());
        assert_eq!(ch.name(), "speech_to_speech");
        assert_eq!(ch.alias(), "desk");
        assert_eq!(
            crate::orchestrator::composite_channel_key(ch.name(), Some(ch.alias())),
            "speech_to_speech.desk"
        );
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

    #[test]
    fn persona_inline_used_when_no_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg();
        c.broker_persona = Some("inline persona".into());
        c.broker_persona_path = None;
        assert_eq!(resolve_persona(&c, dir.path()).unwrap(), "inline persona");
    }

    #[test]
    fn persona_path_read_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("b.md");
        std::fs::write(&p, "file persona").unwrap();
        let mut c = cfg();
        c.broker_persona_path = Some(p.to_string_lossy().into());
        assert_eq!(resolve_persona(&c, dir.path()).unwrap(), "file persona");
    }

    #[test]
    fn persona_path_relative_within_workspace_read_from_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("broker.md"), "relative persona").unwrap();
        let mut c = cfg();
        c.broker_persona_path = Some("broker.md".into());
        assert_eq!(resolve_persona(&c, dir.path()).unwrap(), "relative persona");
    }

    #[test]
    fn persona_path_absolute_outside_workspace_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.md");
        std::fs::write(&secret, "not for Google").unwrap();
        let mut c = cfg();
        c.broker_persona_path = Some(secret.to_string_lossy().into());
        let err = resolve_persona(&c, workspace.path())
            .expect_err("absolute path outside the workspace must be rejected");
        assert!(err.to_string().contains("escapes workspace"), "got: {err}");
    }

    #[test]
    fn persona_path_dotdot_escape_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        let mut c = cfg();
        c.broker_persona_path = Some("../etc/passwd".into());
        let err = resolve_persona(&c, workspace.path())
            .expect_err("`..`-escaping broker_persona_path must be rejected");
        assert!(err.to_string().contains("escapes workspace"), "got: {err}");
    }

    #[test]
    fn voice_defaults_per_model_kind_when_unset() {
        let mut native = cfg();
        native.model_kind = ModelKind::NativeAudio;
        native.voice = None;
        let v = gemini_live::wire::build_setup(&build_broker_setup(&native, "p"));
        assert_eq!(
            v["setup"]["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]["voiceName"],
            "Autonoe"
        );

        let mut half = cfg();
        half.model_kind = ModelKind::HalfCascade;
        half.voice = None;
        let v = gemini_live::wire::build_setup(&build_broker_setup(&half, "p"));
        assert_eq!(
            v["setup"]["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]["voiceName"],
            "Kore"
        );
    }

    #[test]
    fn voice_explicit_override_wins_over_default() {
        let mut c = cfg();
        c.model_kind = ModelKind::NativeAudio;
        c.voice = Some("Puck".into());
        let v = gemini_live::wire::build_setup(&build_broker_setup(&c, "p"));
        assert_eq!(
            v["setup"]["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]["voiceName"],
            "Puck"
        );
    }
}
