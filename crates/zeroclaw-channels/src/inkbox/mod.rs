//! Inkbox channel — email, SMS/MMS, iMessage, and voice for an agent identity.
//!
//! A native port of the Inkbox OpenClaw / Hermes plugins into the ZeroClaw
//! runtime. Inbound email/SMS/iMessage/voice arrive over the **Inkbox
//! tunnel** (the in-repo `inkbox` SDK's `tunnels-runtime` data plane): this
//! channel runs a loopback HTTP/WebSocket server and the tunnel forwards
//! inbound traffic to it, exactly as the Hermes plugin ran a local server
//! behind the tunnel. Outbound replies go back through the Inkbox API.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use inkbox::Inkbox;
use tokio::sync::mpsc;
use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

mod inbound;
mod realtime;
mod voice;

pub use realtime::RealtimeConfig;

/// Loopback host the tunnel forwards inbound traffic to. The SDK's
/// `validate_forward_target` requires a literal loopback address.
const FORWARD_HOST: &str = "127.0.0.1";

/// Native Inkbox channel bound to a single agent identity.
///
/// Holds the blocking [`Inkbox`] client (`Send + Sync`) rather than an
/// `AgentIdentity` facade — the facade is `!Send` (interior `RefCell`), so the
/// identity is resolved fresh inside each blocking call instead.
pub struct InkboxChannel {
    /// Owning Inkbox API client (blocking transport, `Send + Sync`).
    inkbox: Arc<Inkbox>,
    /// Agent identity handle. Also the tunnel name the data plane opens.
    identity: String,
    /// Webhook signing key (`whsec_...`) used to verify inbound events.
    signing_key: String,
    /// ZeroClaw channel alias (the `<alias>` in `[channels.inkbox.<alias>]`).
    alias: String,
    /// OpenAI Realtime bridge config for calls, when enabled + credentialed.
    /// `None` falls back to Inkbox STT/TTS for voice.
    realtime: Option<RealtimeConfig>,
}

impl InkboxChannel {
    /// Build a channel from an already-constructed client and the identity's
    /// config. `Inkbox::new` does no I/O, so it is safe to construct in the
    /// synchronous orchestrator path; network calls happen in `listen`/`send`.
    pub fn new(
        inkbox: Arc<Inkbox>,
        identity: impl Into<String>,
        signing_key: impl Into<String>,
        alias: impl Into<String>,
        realtime: Option<RealtimeConfig>,
    ) -> Self {
        Self {
            inkbox,
            identity: identity.into(),
            signing_key: signing_key.into(),
            alias: alias.into(),
            realtime,
        }
    }
}

/// Point Inkbox at this agent's tunnel so inbound traffic routes to our
/// loopback server: idempotently ensure webhook subscriptions (mail / text /
/// iMessage) target the tunnel's public host, and route inbound calls to the
/// call-media WebSocket. Safe to re-run on every reconnect — an existing
/// subscription for the same `(owner, url, event)` is left untouched.
///
/// # Arguments
/// * `inkbox` - the API client (needed as `&Arc` for `get_identity`).
/// * `handle` - the agent identity handle (also the tunnel name).
///
/// # Returns
/// `Ok(())` once the control-plane routing matches the tunnel, else the first
/// API error encountered.
fn reconcile_routing(inkbox: &Arc<Inkbox>, handle: &str) -> Result<()> {
    let identity = inkbox
        .get_identity(handle)
        .with_context(|| format!("resolve Inkbox identity {handle:?}"))?;
    let public_host = identity
        .tunnel()
        .map(|t| t.public_host)
        .filter(|h| !h.is_empty())
        .ok_or_else(|| anyhow::anyhow!("identity {handle:?} has no tunnel public_host"))?;
    let webhook_url = format!("https://{public_host}/webhook");

    let ns = inkbox.webhooks();
    let subs = ns.subscriptions();
    // Create only when nothing already targets this (owner, url, event). `list`
    // filters by url + event and omits deleted rows, so an empty result means
    // we must create.
    let ensure = |event: &str,
                  mailbox_id: Option<uuid::Uuid>,
                  phone_id: Option<uuid::Uuid>,
                  agent_id: Option<uuid::Uuid>|
     -> Result<()> {
        let existing = subs
            .list(mailbox_id, phone_id, agent_id, Some(webhook_url.as_str()), Some(event))
            .with_context(|| format!("list Inkbox {event} subscriptions"))?;
        if existing.is_empty() {
            subs.create(&webhook_url, &[event.to_string()], mailbox_id, phone_id, agent_id)
                .with_context(|| format!("create Inkbox {event} subscription"))?;
        }
        Ok(())
    };

    if let Some(mailbox) = identity.mailbox() {
        ensure("message.received", Some(mailbox.id), None, None)?;
    }
    if let Some(phone) = identity.phone_number() {
        ensure("text.received", None, Some(phone.id), None)?;
        // Auto-accept inbound calls and bridge their audio to our call-media WS.
        let call_ws = format!("wss://{public_host}/phone/media/ws");
        inkbox
            .phone_numbers()
            .update(
                &phone.id.to_string(),
                Some(Some("auto_accept")),
                Some(Some(call_ws.as_str())),
                None,
                None,
            )
            .context("set Inkbox incoming-call WebSocket URL")?;
    }
    if identity.imessage_enabled() {
        ensure("imessage.received", None, None, Some(identity.id()))?;
    }
    Ok(())
}

impl Attributable for InkboxChannel {
    fn role(&self) -> Role {
        Role::Channel(ChannelKind::Inkbox)
    }

    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for InkboxChannel {
    fn name(&self) -> &str {
        "inkbox"
    }

    /// Show a typing bubble while composing a reply — iMessage only (SMS/email
    /// have no typing indicator). The orchestrator calls this on a refresh loop
    /// (iMessage bubbles expire after a few seconds), so we just send one ping
    /// per call. Best-effort: a typing failure never blocks the reply.
    async fn start_typing(&self, recipient: &str) -> Result<()> {
        let Some(cid) = recipient.strip_prefix("imessage:") else {
            return Ok(());
        };
        let Ok(conversation_id) = uuid::Uuid::parse_str(cid) else {
            return Ok(());
        };
        let inkbox = self.inkbox.clone();
        // Blocking SDK request on the blocking pool; ignore errors.
        let _ =
            tokio::task::spawn_blocking(move || inkbox.imessages().send_typing(&conversation_id))
                .await;
        Ok(())
    }

    /// Send an outbound reply. `recipient` is a tagged target stamped by the
    /// inbound handlers: `"email:<addr>"`, `"sms:<conversation_id>"`, or
    /// `"imessage:<conversation_id>"`. Routes to the matching Inkbox API call.
    async fn send(&self, message: &SendMessage) -> Result<()> {
        // Live call replies go to the open WebSocket as TTS, not the REST API,
        // and need no identity round-trip — handle them before the blocking path.
        // Live-call audio replies (STT/TTS path) go to the open socket; a miss
        // means the call already ended — drop quietly. Post-call reflection
        // turns also target `call:<id>` / `noreply` and need no delivery.
        if message.recipient == "noreply" {
            return Ok(());
        }
        if let Some(conn_id) = message.recipient.strip_prefix("call:") {
            voice::speak_to_call(conn_id, &message.content);
            return Ok(());
        }
        // In-call consult: route the agent's answer back to the realtime bridge.
        if let Some(id) = message.recipient.strip_prefix("consult:") {
            realtime::deliver_consult(id, &message.content);
            return Ok(());
        }

        // Clone everything needed into the blocking task: the AgentIdentity
        // facade is `!Send`, so we resolve and use it entirely on one thread.
        let target = message.recipient.clone();
        let content = message.content.clone();
        let subject = message.subject.clone();
        let in_reply_to = message.in_reply_to.clone();
        let inkbox = self.inkbox.clone();
        let handle = self.identity.clone();

        tokio::task::spawn_blocking(move || -> Result<()> {
            let identity = inkbox
                .get_identity(&handle)
                .with_context(|| format!("resolve Inkbox identity {handle:?}"))?;

            // Split the tagged reply target into (mode, id). Bare strings with
            // no tag fall back to SMS so a hand-built target still routes.
            let (mode, id) = target.split_once(':').unwrap_or(("sms", target.as_str()));
            match mode {
                "email" => {
                    let to = [id.to_string()];
                    identity.send_email(
                        &to,
                        subject.as_deref().unwrap_or("(no subject)"),
                        Some(content.as_str()),
                        None,
                        None,
                        None,
                        in_reply_to.as_deref(),
                        None,
                    )?;
                }
                "sms" => {
                    identity.send_text(None, Some(id), Some(content.as_str()), None)?;
                }
                "smsto" => {
                    // Bare remote number (no conversation row yet): send via
                    // `to`, not `conversation_id`.
                    let to = inkbox::phone::resources::texts::TextRecipients::One(id.to_string());
                    identity.send_text(Some(to), None, Some(content.as_str()), None)?;
                }
                "imessage" => {
                    let cid = uuid::Uuid::parse_str(id).map_err(|e| {
                        anyhow::anyhow!("invalid iMessage conversation id {id:?}: {e}")
                    })?;
                    identity.send_imessage(None, Some(&cid), Some(content.as_str()), None, None)?;
                }
                other => anyhow::bail!("unknown Inkbox reply-target mode {other:?}"),
            }
            Ok(())
        })
        .await
        .context("Inkbox send task panicked")?
    }

    /// Run the inbound side: bind a loopback HTTP/WebSocket server, then open
    /// the Inkbox tunnel pointed at it. The tunnel runtime serves forever
    /// (reconnecting internally); this returns only when the tunnel or the
    /// loopback server ends, letting the orchestrator respawn the listener.
    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Bind an ephemeral loopback port for the tunnel to forward to.
        let listener = tokio::net::TcpListener::bind((FORWARD_HOST, 0))
            .await
            .context("bind Inkbox loopback server")?;
        let port = listener
            .local_addr()
            .context("read Inkbox loopback addr")?
            .port();
        let forward_to = format!("http://{FORWARD_HOST}:{port}");

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            format!(
                "[inkbox] inbound server listening on {forward_to}; opening tunnel for identity {}",
                self.identity
            ),
        );

        // Loopback server that receives the tunnel-forwarded webhooks + call WS.
        let app = inbound::router(inbound::AppState {
            tx,
            signing_key: self.signing_key.clone(),
            alias: self.alias.clone(),
            realtime: self.realtime.clone(),
            inkbox: self.inkbox.clone(),
            identity: self.identity.clone(),
        });
        let server = tokio::spawn(async move { axum::serve(listener, app).await });

        // Point Inkbox at this tunnel (idempotent) before opening the data
        // plane, so inbound mail/text/iMessage/calls actually route here.
        //
        // The blocking Inkbox SDK drives its own tokio runtime internally, so it
        // must run on a plain OS thread — `spawn_blocking` worker threads belong
        // to the daemon runtime, and dropping an inner runtime from one panics
        // with "Cannot drop a runtime in a context where blocking is not
        // allowed". We bridge each blocking call back to async via a oneshot.
        {
            let inkbox = self.inkbox.clone();
            let handle = self.identity.clone();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let _ = done_tx.send(reconcile_routing(&inkbox, &handle));
            });
            done_rx
                .await
                .context("Inkbox routing setup thread dropped")?
                .context("set up Inkbox inbound routing")?;
        }

        // Open the tunnel on a dedicated OS thread (same nested-runtime reason).
        // `connect` blocks, driving its own runtime to completion (reconnecting
        // internally), and returns only when the tunnel ends.
        let (tunnel_tx, tunnel_rx) = tokio::sync::oneshot::channel();
        {
            let inkbox = self.inkbox.clone();
            let handle = self.identity.clone();
            std::thread::spawn(move || {
                let _ = tunnel_tx.send(inkbox.tunnels().connect(&handle, &forward_to));
            });
        }

        // Whichever side ends first ends the listen loop.
        tokio::select! {
            r = tunnel_rx => match r {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(anyhow::Error::new(e).context("Inkbox tunnel runtime exited")),
                Err(_) => Err(anyhow::anyhow!("Inkbox tunnel thread ended without a result")),
            },
            r = server => match r {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(anyhow::Error::new(e).context("Inkbox loopback server exited")),
                Err(e) => Err(anyhow::Error::new(e).context("Inkbox server task failed")),
            },
        }
    }
}
