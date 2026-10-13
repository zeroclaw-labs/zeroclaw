//! Speech-to-speech broker channel.
//!
//! Bridges a hosted bidirectional voice model (e.g. Gemini Live) into
//! ZeroClaw as a broker channel: audio in, transcript/audio out, with a
//! broker persona steering how the model mediates the call. This module
//! currently holds only the `Channel` skeleton — the audio seam and session
//! handle land in a later task.

use std::collections::VecDeque;
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
        // Operator-pinned model id when set; otherwise `model`'s default for
        // the kind. The api-version stays derived from `model`, so an id that
        // does not match the kind is the provider's to reject (Gemini returns
        // a setup error), not this crate's to guess.
        model_id_override: {
            let id = cfg.model.trim();
            (!id.is_empty()).then(|| id.to_string())
        },
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

/// One live call on an alias: the text-send handle plus the identity and the
/// delivery bookkeeping that belong to that call and not to the alias.
///
/// `id` is minted per attach and travels in every `ChannelMessage` this call
/// produces (`sender`, `reply_target`), which is what keeps two calls on the
/// same alias in separate conversation histories and lets [`Channel::send`]
/// tell a reply for *this* call from one that belongs to a call already over.
///
/// `pending_text` and `pending_acks` hold the non-audio controls the caller
/// enqueued but the provider has not confirmed. The transport drops whatever
/// is queued while it reconnects (audio is stale by then, and the crate
/// leaves recovery to its caller), so without this the agent's settled reply
/// or a function acknowledgement could vanish with `send()` already having
/// reported success.
struct ActiveSession {
    id: String,
    sender: ClientTextSender,
    /// Replies enqueued since the last confirmed turn, oldest first, kept so
    /// a reopen can re-send them. Bounded: a long outage must not grow this
    /// without limit, and re-speaking a stale backlog is worse than losing
    /// its oldest entries.
    pending_text: VecDeque<String>,
    /// Tool-call ids acknowledged into the connection that has since died.
    /// These are never replayed — the call they answer belonged to that
    /// connection — but their loss is reported with the id so the operator
    /// can correlate a model turn that never got its answer.
    pending_acks: Vec<String>,
}

/// How many un-confirmed replies one call keeps for re-send across a reopen.
/// Four covers the realistic case (a reply or two in flight when the socket
/// drops) without letting a long outage queue a monologue.
const MAX_PENDING_TEXT: usize = 4;

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
    ///
    /// The entry carries the call's own identity alongside the handle: a
    /// reply that names an earlier call must not be spoken into the one
    /// running now, and each call keeps its own conversation history.
    active_session: Arc<Mutex<Option<ActiveSession>>>,
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
        *self.active_session.lock().unwrap() = Some(ActiveSession {
            id: uuid::Uuid::new_v4().to_string(),
            sender: session.text_sender(),
            pending_text: VecDeque::new(),
            pending_acks: Vec::new(),
        });
    }

    /// The identity of the call currently attached to this alias, if any.
    /// Messages carry it so history and reply routing follow the call rather
    /// than the alias.
    fn session_id(&self) -> Option<String> {
        self.active_session
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.id.clone())
    }

    /// The reply address of one call: what the orchestrator echoes back as
    /// `SendMessage::recipient`, and what [`Channel::send`] matches against
    /// the live call before speaking anything into it.
    fn reply_target_for(&self, session_id: &str) -> String {
        format!("{}:{session_id}", self.alias)
    }

    /// Hold a tool-call id as unanswered until its write returns.
    fn mark_ack_pending(&self, id: &str) {
        if let Some(active) = self.active_session.lock().unwrap().as_mut() {
            active.pending_acks.push(id.to_string());
        }
    }

    /// Release an id once its acknowledgement left the transport (or failed
    /// outright, which is reported by the caller).
    fn clear_ack_pending(&self, id: &str) {
        if let Some(active) = self.active_session.lock().unwrap().as_mut() {
            active.pending_acks.retain(|pending| pending != id);
        }
    }

    /// React to a transparent reconnect: everything the caller enqueued
    /// during the outage was discarded by the transport, so re-send the
    /// replies that have not been confirmed and report the acknowledgements
    /// that cannot be (their tool call died with the old connection).
    ///
    /// Re-sending replies rather than dropping them is the recoverable half
    /// the agent cannot redo on its own: it already considers that turn
    /// answered.
    ///
    /// `resumed` decides whether re-sending is safe, and it is the reason this
    /// is not the blind replay the transport crate warns against. A fresh
    /// reopen (`resumed == false`) threw the turn away with the connection, so
    /// nothing the caller heard survives and every unconfirmed reply has to be
    /// spoken again. A resumed reopen kept the turn server-side, where a reply
    /// that reached the socket first may already have been spoken; re-sending
    /// there would race the provider's own resume and repeat a sentence to the
    /// caller, so the loss is reported instead.
    async fn replay_controls_after_reopen(&self, session: &Session, resumed: bool) {
        let (replies, lost_acks, session_id) = {
            let mut guard = self.active_session.lock().unwrap();
            match guard.as_mut() {
                Some(active) => (
                    active.pending_text.iter().cloned().collect::<Vec<_>>(),
                    std::mem::take(&mut active.pending_acks),
                    active.id.clone(),
                ),
                None => return,
            }
        };
        if !lost_acks.is_empty() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "alias": self.alias,
                        "session": session_id,
                        "tool_call_ids": lost_acks,
                    })),
                "speech_to_speech: tool acknowledgements lost with the previous connection"
            );
        }
        if replies.is_empty() {
            return;
        }
        if resumed {
            // The provider restored the turn state server-side, so a reply
            // that reached the socket before it died may already have been
            // spoken. Re-sending here is exactly the blind replay that races
            // the server's own turn-resume and can repeat a sentence to the
            // caller, so report what is unconfirmed and let the turn stand.
            if let Some(active) = self.active_session.lock().unwrap().as_mut() {
                active.pending_text.clear();
            }
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "alias": self.alias,
                        "session": session_id,
                        "unconfirmed": replies.len(),
                    })),
                "speech_to_speech: provider resumed the turn; unconfirmed replies left unsent"
            );
            return;
        }
        let sender = session.text_sender();
        let mut delivered = 0usize;
        for reply in &replies {
            match sender.send_client_text(reply).await {
                Ok(()) => delivered += 1,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "alias": self.alias,
                                "session": session_id,
                                "error": e.to_string(),
                            })),
                        "speech_to_speech: could not re-send a reply after reconnect"
                    );
                    break;
                }
            }
        }
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "alias": self.alias,
                    "session": session_id,
                    "resent": delivered,
                    "queued": replies.len(),
                    "provider_resumed": resumed,
                })
            ),
            "speech_to_speech: re-sent unconfirmed replies after reconnect"
        );
    }

    /// Build a `ChannelMessage` carrying one call's history-scope identity:
    /// `channel` = `"speech_to_speech"`, `channel_alias` = this alias,
    /// `sender` = `"speech_to_speech:{alias}:{session}"`, `reply_target` =
    /// `"{alias}:{session}"`, `conversation_scope` = `Sender`, no
    /// `thread_ts`. `id` and `content` are the only fields callers vary —
    /// neither feeds `orchestrator::conversation_history_key`, so any two
    /// messages built here **for the same call** resolve to the same history
    /// bucket, while a later call on the same alias gets its own.
    ///
    /// Carrying the call identity (and not just the alias) is what keeps a
    /// phone call's conversation to itself and what lets [`Channel::send`]
    /// reject a reply addressed to a call that has already ended: the
    /// orchestrator echoes `reply_target` back as `SendMessage::recipient`.
    /// This is the single source of truth for that identity:
    /// [`Self::consult_message`] and [`Self::history_key_for_transcript`]
    /// both build through it, which is what guarantees a future transcript
    /// handler and the consult turn of the same call land in the same
    /// conversation history (see [`Self::history_key_for_sender`]).
    fn scoped_message(&self, id: impl Into<String>, content: impl Into<String>) -> ChannelMessage {
        // No live call (a message built outside a session, as the history-key
        // helpers do) keeps the bare alias identity rather than inventing one.
        let session = self.session_id();
        let (sender, reply_target) = match session.as_deref() {
            Some(sid) => (
                format!("speech_to_speech:{}:{sid}", self.alias),
                self.reply_target_for(sid),
            ),
            None => (
                format!("speech_to_speech:{}", self.alias),
                self.alias.clone(),
            ),
        };
        ChannelMessage {
            channel_alias: Some(self.alias.clone()),
            explicitly_addressed: true,
            conversation_scope: ChannelConversationScope::Sender,
            ..ChannelMessage::new(
                id,
                sender,
                reply_target,
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
                            // The id is held as outstanding until the write
                            // returns, so a connection that dies underneath
                            // reports which call went unanswered instead of
                            // dropping the ack silently.
                            self.mark_ack_pending(&id);
                            match session.send_tool_response(&id).await {
                                Ok(()) => self.clear_ack_pending(&id),
                                Err(e) => {
                                    self.clear_ack_pending(&id);
                                    ::zeroclaw_log::record!(
                                        WARN,
                                        ::zeroclaw_log::Event::new(
                                            module_path!(),
                                            ::zeroclaw_log::Action::Note
                                        )
                                        .with_attrs(::serde_json::json!({
                                            "alias": self.alias,
                                            "tool_call_id": id,
                                            "error": e.to_string(),
                                        })),
                                        "speech_to_speech: tool acknowledgement was not delivered"
                                    );
                                }
                            }
                            if name == "end_session" {
                                break;
                            }
                        }
                        Some(Event::SessionOpened { is_reconnect: true, resumed }) => {
                            // The transport dropped whatever was queued while
                            // it reconnected. Audio stays lossy by design, but
                            // the controls have defined outcomes: replies are
                            // re-sent on the fresh connection, acknowledgements
                            // are not (the call they answer belonged to the
                            // dead one) and are reported with their id.
                            self.replay_controls_after_reopen(&session, resumed).await;
                        }
                        Some(Event::TurnComplete) | Some(Event::Interrupted) => {
                            // The model consumed the turn, so nothing relayed
                            // before it still needs re-sending.
                            if let Some(active) = self.active_session.lock().unwrap().as_mut() {
                                active.pending_text.clear();
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
        // unconditional clear is safe on every break path above. A reply the
        // model never confirmed dies with the call — the caller hung up or the
        // provider gave up before hearing it — and that is a delivery failure
        // the orchestrator was told did not happen, so say so here.
        let ended = self.active_session.lock().unwrap().take();
        if let Some(active) = ended
            && (!active.pending_text.is_empty() || !active.pending_acks.is_empty())
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "alias": self.alias,
                        "session": active.id,
                        "unconfirmed_replies": active.pending_text.len(),
                        "unanswered_tool_calls": active.pending_acks,
                    })),
                "speech_to_speech: call ended with control messages still unconfirmed"
            );
        }
        Ok(())
    }
}

impl ::zeroclaw_api::attribution::Attributable for SpeechToSpeechChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::SpeechToSpeech,
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
    /// output.
    ///
    /// The reply is delivered only to the call it was written for: the
    /// orchestrator echoes the inbound `reply_target` back as
    /// `SendMessage::recipient`, and a recipient naming a call that has
    /// ended (or a different one that has since started on this alias) is
    /// dropped with a log rather than spoken to whoever is on the line now.
    ///
    /// If no session is active (call already ended, or none ever started),
    /// this logs and returns `Ok(())` rather than erroring — a reply arriving
    /// after hangup is a race, not a failure. The same treatment applies if
    /// a session *was* attached but died between attach and this call
    /// (`SessionError::Closed`): only genuinely-unexpected relay failures
    /// surface as `Err`.
    ///
    /// A reply that reaches the transport is also remembered until the model
    /// confirms the turn, so a reconnect can re-send it instead of silently
    /// swallowing it (see [`ActiveSession`]).
    async fn send(&self, message: &SendMessage) -> Result<()> {
        let sender = {
            let mut guard = self.active_session.lock().unwrap();
            match guard.as_mut() {
                Some(active) => {
                    let expected = format!("{}:{}", self.alias, active.id);
                    // An empty recipient means the caller did not route by
                    // reply target at all (internal notices); those still go
                    // to the live call. Anything else must name this call.
                    if !message.recipient.is_empty()
                        && message.recipient != expected
                        && message.recipient != self.alias
                    {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({
                                "alias": self.alias,
                                "recipient": message.recipient,
                                "active_session": active.id,
                            })),
                            "speech_to_speech: reply addressed to another session; dropping"
                        );
                        return Ok(());
                    }
                    if active.pending_text.len() == MAX_PENDING_TEXT {
                        // Dropping the oldest keeps a long outage from queueing
                        // a monologue, but it is still a reply that may never
                        // be spoken — say so rather than losing it quietly,
                        // which is the failure this bookkeeping exists to end.
                        active.pending_text.pop_front();
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({
                                "alias": self.alias,
                                "session": active.id,
                                "kept": MAX_PENDING_TEXT,
                            })),
                            "speech_to_speech: unconfirmed reply backlog is full; \
                             the oldest reply will not be re-sent"
                        );
                    }
                    active.pending_text.push_back(message.content.clone());
                    Some(active.sender.clone())
                }
                None => None,
            }
        };
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
        // Inert until the audio-frame transport lands (a later PR): there is no
        // inbound source to drive yet. Park until the listener supervisor
        // cancels this task (config reload / shutdown). Returning `Ok(())` here
        // would make the supervisor treat the channel as an unexpected exit and
        // restart it in a backoff loop, churning an enabled-but-staged channel.
        std::future::pending::<()>().await;
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

    /// A reconnector that hands out `next` once and then refuses, so a test
    /// can script exactly one transparent reopen.
    fn once(next: FakeTransport) -> Reconnector<FakeTransport> {
        let mut slot = Some(next);
        Box::new(move || {
            let t = slot.take();
            Box::pin(async move {
                t.ok_or(SessionError::Transport(TransportError::Connect(
                    "drained".into(),
                )))
            })
        })
    }

    /// Poll `sent` until `needle` shows up, or give up: the driver task writes
    /// enqueued commands asynchronously.
    async fn wait_for_sent(sent: &Arc<Mutex<Vec<String>>>, needle: &str) -> bool {
        for _ in 0..2000 {
            if sent.lock().unwrap().iter().any(|t| t.contains(needle)) {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        false
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

        let target = ch.reply_target_for(&ch.session_id().expect("session attached"));
        ch.send(&SendMessage::new("You have a 3pm sync.", &target))
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
    async fn controls_unconfirmed_at_a_reconnect_keep_a_defined_outcome() {
        // Driven through `run_session`, the way production does: the transport
        // drops whatever it holds while reconnecting, and `send()` has already
        // told the orchestrator the reply was accepted. The two control classes
        // therefore get defined, different outcomes — a settled reply that the
        // model never confirmed is spoken again on the reopened connection,
        // while a tool acknowledgement is never replayed: the call it answers
        // died with the old connection.
        let mut first = FakeTransport::new(false);
        first.push_data(br#"{"setupComplete":{}}"#.to_vec());
        first.push_data(
            br#"{"toolCall":{"functionCalls":[{"name":"consult_agent","id":"call-1","args":{"prompt":"what time is it"}}]}}"#.to_vec(),
        );
        first.push_close(1011, "server restart");
        let second = FakeTransport::new(true);
        let second_sent = second.sent.clone();

        let session = Session::connect_with_transport(client_config(&cfg()), first, once(second))
            .await
            .unwrap();
        let ch = Arc::new(SpeechToSpeechChannel::new("desk".to_string()));
        let (tx, mut rx) = mpsc::channel(8);
        let driver = {
            let ch = Arc::clone(&ch);
            tokio::spawn(async move { ch.run_session(session, tx).await })
        };

        // The loop attaches the call before it processes anything, so the
        // reply can be addressed to it the way the orchestrator would.
        let target = loop {
            if let Some(id) = ch.session_id() {
                break ch.reply_target_for(&id);
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        };
        // The consult turn reached the orchestrator: that is the tool call
        // whose acknowledgement belongs to the connection about to die.
        let consult = rx.recv().await.expect("consult turn must be relayed");
        assert_eq!(consult.content, "what time is it");

        ch.send(&SendMessage::new("it is three o'clock", &target))
            .await
            .expect("relaying a reply is not itself a failure");

        assert!(
            wait_for_sent(&second_sent, "it is three o'clock").await,
            "a reply the model never confirmed must be spoken again after the              reconnect, got {:?}",
            second_sent.lock().unwrap()
        );
        assert!(
            !second_sent
                .lock()
                .unwrap()
                .iter()
                .any(|frame| frame.contains("call-1")),
            "the acknowledgement of a call from the dead connection must not be              replayed, got {:?}",
            second_sent.lock().unwrap()
        );

        ch.stop().await;
        driver.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_failed_resend_after_reconnect_does_not_break_the_call() {
        // Same outage, but the reopened connection dies too, so the re-send
        // write fails. The broker reports the loss and lets the session end
        // instead of unwinding the call or claiming delivery.
        let mut first = FakeTransport::new(false);
        first.push_data(br#"{"setupComplete":{}}"#.to_vec());
        first.push_close(1011, "server restart");
        let mut second = FakeTransport::new(false);
        second.push_data(br#"{"setupComplete":{}}"#.to_vec());
        second.push_close(1011, "gone again");
        let second_sent = second.sent.clone();

        let session = Session::connect_with_transport(client_config(&cfg()), first, once(second))
            .await
            .unwrap();
        let ch = Arc::new(SpeechToSpeechChannel::new("desk".to_string()));
        let (tx, _rx) = mpsc::channel(8);
        let driver = {
            let ch = Arc::clone(&ch);
            tokio::spawn(async move { ch.run_session(session, tx).await })
        };
        let target = loop {
            if let Some(id) = ch.session_id() {
                break ch.reply_target_for(&id);
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        };
        let _ = ch.send(&SendMessage::new("settled reply", &target)).await;

        // The session ends once reconnection is exhausted; the loop returns
        // normally rather than propagating the failed write.
        driver.await.unwrap().unwrap();
        assert!(
            !second_sent
                .lock()
                .unwrap()
                .iter()
                .any(|frame| frame.contains("settled reply")),
            "a write that never left must not look delivered, got {:?}",
            second_sent.lock().unwrap()
        );
        // The call is over, so a later reply is the documented no-op.
        ch.send(&SendMessage::new("later", &target))
            .await
            .expect("a send after the session ended must not error");
    }

    #[tokio::test]
    async fn a_reply_for_an_ended_call_is_not_spoken_into_the_next_one() {
        // Two calls run one after another on the same alias. A reply that was
        // written for the first must not be delivered to whoever is on the
        // line now, and the two calls must not share a conversation history.
        let first = FakeTransport::new(true);
        let first_session =
            Session::connect_with_transport(client_config(&cfg()), first, no_reconnect())
                .await
                .unwrap();
        let ch = SpeechToSpeechChannel::new("desk".to_string());
        ch.attach_session(&first_session);
        let first_id = ch.session_id().expect("first call attached");
        let first_target = ch.reply_target_for(&first_id);
        let first_history = ch.history_key_for_sender();

        let second = FakeTransport::new(true);
        let second_sent = second.sent.clone();
        let second_session =
            Session::connect_with_transport(client_config(&cfg()), second, no_reconnect())
                .await
                .unwrap();
        ch.attach_session(&second_session);
        let second_id = ch.session_id().expect("second call attached");
        let second_target = ch.reply_target_for(&second_id);
        let second_history = ch.history_key_for_sender();

        assert_ne!(first_id, second_id, "each call gets its own identity");
        assert_ne!(
            first_history, second_history,
            "two calls on one alias must not share a conversation history"
        );

        ch.send(&SendMessage::new(
            "answer for the first caller",
            &first_target,
        ))
        .await
        .expect("a late reply is not a failure");
        ch.send(&SendMessage::new(
            "answer for the second caller",
            &second_target,
        ))
        .await
        .unwrap();

        assert!(
            wait_for_sent(&second_sent, "answer for the second caller").await,
            "the live call must receive its own reply"
        );
        assert!(
            !second_sent
                .lock()
                .unwrap()
                .iter()
                .any(|frame| frame.contains("answer for the first caller")),
            "a reply addressed to the previous call must never reach this one, got {:?}",
            second_sent.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn send_with_no_active_session_is_a_noop() {
        let ch = SpeechToSpeechChannel::new("desk".to_string());
        ch.send(&SendMessage::new("hello", "desk"))
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
    fn build_broker_setup_threads_configured_model_id() {
        let mut c = cfg();
        c.model = "gemini-2.5-flash-native-audio-preview-12-2025".into();
        assert_eq!(
            build_broker_setup(&c, "p").model_id_override.as_deref(),
            Some("gemini-2.5-flash-native-audio-preview-12-2025")
        );
        // An empty configured model falls back to the kind default (no override).
        let mut c2 = cfg();
        c2.model = String::new();
        assert_eq!(build_broker_setup(&c2, "p").model_id_override, None);
    }

    #[tokio::test]
    async fn listen_parks_until_cancelled_not_immediate_ok() {
        let ch = SpeechToSpeechChannel::new("desk");
        let (tx, _rx) = mpsc::channel(1);
        // An immediate `Ok(())` would make the listener supervisor treat the
        // channel as a crashed listener and restart it in a loop. `listen()`
        // must instead stay parked (here: still pending after a short wait)
        // until the supervisor cancels it.
        let parked =
            tokio::time::timeout(std::time::Duration::from_millis(50), ch.listen(tx)).await;
        assert!(
            parked.is_err(),
            "listen() should stay parked until cancelled, not return on its own"
        );
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
