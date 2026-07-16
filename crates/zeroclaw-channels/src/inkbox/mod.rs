//! Inkbox channel — email, SMS/MMS, iMessage, and voice for an agent identity.
//!
//! A native Inkbox integration for the ZeroClaw runtime. Inbound
//! email/SMS/iMessage/voice arrive over the **Inkbox tunnel** (the `inkbox`
//! SDK's `tunnels-runtime` data plane): this channel runs a loopback
//! HTTP/WebSocket server and the tunnel forwards inbound traffic to it.
//! Outbound replies go back through the Inkbox API.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use inkbox::Inkbox;
use tokio::sync::mpsc;
use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

mod delivery_failure;
mod inbound;
mod realtime;
mod voice;

pub use realtime::RealtimeConfig;

/// Loopback host the tunnel forwards inbound traffic to. The SDK's
/// `validate_forward_target` requires a literal loopback address.
const FORWARD_HOST: &str = "127.0.0.1";
const INKBOX_TYPING_REFRESH_SECS: u64 = 40;

/// Seconds since the Unix epoch, for `ChannelMessage` timestamps. Shared by the
/// inbound webhook + call-media handlers.
pub(super) fn now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A parsed reply-target routing decision. The inbound handlers stamp
/// `recipient` as `"<mode>:<id>"`; this classifies it (a bare string with no
/// tag falls back to SMS so a hand-built target still routes). Pure + unit-tested.
enum ReplyRoute<'a> {
    /// Post-call reflection turns and other non-delivery targets.
    Noreply,
    /// Live STT/TTS call leg (`conn_id`).
    Call(&'a str),
    /// In-call consult answer (`consult id`).
    Consult(&'a str),
    Email(&'a str),
    Sms(&'a str),
    SmsTo(&'a str),
    Imessage(&'a str),
    /// Tagged with an unrecognized mode.
    Unknown(&'a str),
}

fn reply_route(target: &str) -> ReplyRoute<'_> {
    if target == "noreply" {
        return ReplyRoute::Noreply;
    }
    if let Some(c) = target.strip_prefix("call:") {
        return ReplyRoute::Call(c);
    }
    if let Some(c) = target.strip_prefix("consult:") {
        return ReplyRoute::Consult(c);
    }
    let (mode, id) = target.split_once(':').unwrap_or(("sms", target));
    match mode {
        "email" => ReplyRoute::Email(id),
        "sms" => ReplyRoute::Sms(id),
        "smsto" => ReplyRoute::SmsTo(id),
        "imessage" => ReplyRoute::Imessage(id),
        _ => ReplyRoute::Unknown(mode),
    }
}

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
    /// Delivery-failure retry loop: shared budget between the send path and
    /// the inbound webhook server, so both failure surfaces draw one cap.
    failure: Arc<delivery_failure::FailureTracker>,
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
        let alias = alias.into();
        Self {
            inkbox,
            identity: identity.into(),
            signing_key: signing_key.into(),
            failure: Arc::new(delivery_failure::FailureTracker::new(alias.clone())),
            alias,
            realtime,
        }
    }
}

/// Point Inkbox at this agent's tunnel so inbound traffic routes to our
/// loopback server: idempotently ensure webhook subscriptions (mail / text /
/// iMessage) target the tunnel's public host, and route inbound calls to the
/// call-media WebSocket. Safe to re-run on every reconnect — an existing
/// subscription for the same `(owner, url)` with the desired event set is
/// left untouched; one with a drifted event set is patched in place (the
/// server enforces one subscription per `(owner, url)`), so deployments that
/// predate the delivery-failure events pick them up on the next start.
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
        .ok_or_else(|| {
            anyhow::Error::msg(format!("identity {handle:?} has no tunnel public_host"))
        })?;
    let webhook_url = format!("https://{public_host}/webhook");

    let ns = inkbox.webhooks();
    let subs = ns.subscriptions();
    // Reconcile the owner's subscription to the desired event set: adopt a
    // matching row verbatim, patch a drifted event set in place (never
    // delete-recreate — the owner must keep a receiver throughout), create
    // only when nothing targets this (owner, url) yet.
    let ensure = |events: &[&str],
                  mailbox_id: Option<uuid::Uuid>,
                  phone_id: Option<uuid::Uuid>,
                  agent_id: Option<uuid::Uuid>|
     -> Result<()> {
        let existing = subs
            .list(
                mailbox_id,
                phone_id,
                agent_id,
                Some(webhook_url.as_str()),
                None,
            )
            .with_context(|| format!("list Inkbox {events:?} subscriptions"))?;
        let desired: Vec<String> = events.iter().map(|e| e.to_string()).collect();
        match existing.first() {
            // Compare as sets: the server may reorder the stored event list.
            Some(sub)
                if sub.event_types.len() == desired.len()
                    && desired.iter().all(|e| sub.event_types.contains(e)) => {}
            Some(sub) => {
                subs.update(sub.id, None, Some(&desired), None)
                    .with_context(|| format!("update Inkbox {events:?} subscription"))?;
            }
            None => {
                subs.create(&webhook_url, &desired, mailbox_id, phone_id, agent_id, None)
                    .with_context(|| format!("create Inkbox {events:?} subscription"))?;
            }
        }
        Ok(())
    };

    if let Some(mailbox) = identity.mailbox() {
        // Bounce/failure transitions feed the delivery-failure retry loop.
        // Success transitions stay unsubscribed — no consumer for them.
        ensure(
            &["message.received", "message.bounced", "message.failed"],
            Some(mailbox.id),
            None,
            None,
        )?;
    }
    if let Some(phone) = identity.phone_number() {
        // `delivered` resets the retry budget; `delivery_failed` draws it down.
        ensure(
            &["text.received", "text.delivered", "text.delivery_failed"],
            None,
            Some(phone.id),
            None,
        )?;
        // Route inbound calls through the incoming-call webhook (not auto_accept)
        // so Inkbox hits `/incoming-call`, which answers with a call-media WS URL
        // carrying `?call_id=`. That id is what lets the realtime bridge resolve
        // the caller's contact card; auto_accept connects the WS without it.
        let call_ws = format!("wss://{public_host}/phone/media/ws");
        let call_webhook = format!("https://{public_host}/incoming-call");
        inkbox
            .phone_numbers()
            .update(
                &phone.id.to_string(),
                Some(Some("webhook")),
                Some(Some(call_ws.as_str())),
                Some(Some(call_webhook.as_str())),
                None,
            )
            .context("set Inkbox incoming-call webhook + WebSocket URL")?;
    }
    if identity.imessage_enabled() {
        ensure(
            &[
                "imessage.received",
                "imessage.delivered",
                "imessage.delivery_failed",
            ],
            None,
            None,
            Some(identity.id()),
        )?;
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

    fn typing_refresh_secs(&self) -> u64 {
        INKBOX_TYPING_REFRESH_SECS
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
        // Blocking SDK request on the blocking pool. Best-effort: a typing
        // failure must never block the reply, but log at debug so a persistently
        // failing indicator is diagnosable.
        if let Ok(Err(e)) =
            tokio::task::spawn_blocking(move || inkbox.imessages().send_typing(&conversation_id))
                .await
        {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                format!("[inkbox] iMessage typing indicator failed: {e}"),
            );
        }
        Ok(())
    }

    /// Send an outbound reply. `recipient` is a tagged target stamped by the
    /// inbound handlers: `"email:<addr>"`, `"sms:<conversation_id>"`, or
    /// `"imessage:<conversation_id>"`. Routes to the matching Inkbox API call.
    async fn send(&self, message: &SendMessage) -> Result<()> {
        // The delivery-failure wake-up offers `[SILENT]` as its escape hatch:
        // an exact-match reply means "nothing sensible to send" and must not
        // reach the recipient.
        if message.content.trim() == "[SILENT]" {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                format!(
                    "[inkbox] suppressed [SILENT] reply to {}",
                    delivery_failure::mask_target(&message.recipient)
                ),
            );
            return Ok(());
        }

        // Non-REST targets are handled before the blocking path: live-call audio
        // replies go to the open socket (a miss means the call already ended —
        // drop quietly), consult answers go back to the realtime bridge, and
        // post-call reflection turns (`noreply`) need no delivery.
        match reply_route(&message.recipient) {
            ReplyRoute::Noreply => return Ok(()),
            ReplyRoute::Call(conn_id) => {
                if !voice::speak_to_call(conn_id, &message.content) {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        format!("[inkbox] reply for call {conn_id} dropped — leg already ended"),
                    );
                }
                return Ok(());
            }
            ReplyRoute::Consult(id) => {
                realtime::deliver_consult(id, &message.content);
                return Ok(());
            }
            // Delivery targets fall through to the blocking REST path below.
            _ => {}
        }

        // Clone everything needed into the blocking task: the AgentIdentity
        // facade is `!Send`, so we resolve and use it entirely on one thread.
        let target = message.recipient.clone();
        let content = message.content.clone();
        let subject = message.subject.clone();
        let in_reply_to = message.in_reply_to.clone();
        let inkbox = self.inkbox.clone();
        let handle = self.identity.clone();

        let result = tokio::task::spawn_blocking(move || -> Result<()> {
            let identity = inkbox
                .get_identity(&handle)
                .with_context(|| format!("resolve Inkbox identity {handle:?}"))?;

            match reply_route(&target) {
                ReplyRoute::Email(id) => {
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
                        false,
                    )?;
                }
                ReplyRoute::Sms(id) => {
                    identity.send_text(None, Some(id), Some(content.as_str()), None)?;
                }
                ReplyRoute::SmsTo(id) => {
                    // Bare remote number (no conversation row yet): send via
                    // `to`, not `conversation_id`.
                    let to = inkbox::phone::resources::texts::TextRecipients::One(id.to_string());
                    identity.send_text(Some(to), None, Some(content.as_str()), None)?;
                }
                ReplyRoute::Imessage(id) => {
                    let cid = uuid::Uuid::parse_str(id).map_err(|e| {
                        anyhow::Error::msg(format!("invalid iMessage conversation id {id:?}: {e}"))
                    })?;
                    identity.send_imessage(None, Some(&cid), Some(content.as_str()), None, None)?;
                }
                ReplyRoute::Unknown(mode) => {
                    anyhow::bail!("unknown Inkbox reply-target mode {mode:?}")
                }
                // Non-delivery targets were handled before the blocking hop.
                ReplyRoute::Noreply | ReplyRoute::Call(_) | ReplyRoute::Consult(_) => {}
            }
            Ok(())
        })
        .await
        .context("Inkbox send task panicked")?;

        // Send-time failure surface of the delivery-failure retry loop: a
        // rejected send (content policy, opt-out, bad address) wakes the agent
        // to fix and resend. The error still bubbles so the orchestrator logs
        // the failed delivery as usual.
        if let Err(e) = &result {
            self.failure
                .note_send_rejection(&message.recipient, &message.content, e);
        }
        result
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

        // Resolve the tunnel public host up front (blocking SDK on its own OS
        // thread, per the nested-runtime constraint below) so the incoming-call
        // webhook handler can build the call WS URL with `?call_id=`.
        let public_host = {
            let inkbox = self.inkbox.clone();
            let handle = self.identity.clone();
            let (host_tx, host_rx) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let host = inkbox
                    .get_identity(&handle)
                    .ok()
                    .and_then(|id| id.tunnel().map(|t| t.public_host))
                    .filter(|h| !h.is_empty())
                    .unwrap_or_default();
                let _ = host_tx.send(host);
            });
            host_rx.await.unwrap_or_default()
        };
        if public_host.is_empty() {
            // Without it, the incoming-call answer URL is malformed and the
            // realtime bridge can't resolve callers — surface the broken lookup.
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "[inkbox] could not resolve tunnel public_host; incoming-call routing will be degraded",
            );
        }

        // Give the delivery-failure loop the inbound sink so both failure
        // surfaces (send rejections + delivery webhooks) can wake the agent.
        self.failure.set_sender(tx.clone());

        // Loopback server that receives the tunnel-forwarded webhooks + call WS.
        let app = inbound::router(inbound::AppState {
            tx,
            failure: self.failure.clone(),
            signing_key: self.signing_key.clone(),
            alias: self.alias.clone(),
            realtime: self.realtime.clone(),
            inkbox: self.inkbox.clone(),
            identity: self.identity.clone(),
            public_host,
        });
        let server = zeroclaw_spawn::spawn!(async move { axum::serve(listener, app).await });

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
                Err(_) => {
                    Err(anyhow::Error::msg("Inkbox tunnel thread ended without a result"))
                }
            },
            r = server => match r {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(anyhow::Error::new(e).context("Inkbox loopback server exited")),
                Err(e) => Err(anyhow::Error::new(e).context("Inkbox server task failed")),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Log lines must not leak recipient identifiers in cleartext: the
    /// `[SILENT]` suppression log masks the target down to its last 4 chars.
    #[test]
    fn silent_suppression_log_masks_the_recipient() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        // Client construction spins reqwest's blocking runtime; keep it (and
        // its eventual drop) off the test runtime's context.
        let inkbox = std::thread::spawn(|| Inkbox::new("ApiKey_test").expect("client builds"))
            .join()
            .expect("client thread");
        let channel = InkboxChannel::new(inkbox, "ident", "whsec_test", "zc", None);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(channel.send(&SendMessage::new("[SILENT]", "smsto:+15551230000")))
            .expect("suppressed send returns Ok");

        let mut suppression_log = None;
        while let Ok(value) = rx.try_recv() {
            let msg = value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            if msg.contains("suppressed [SILENT]") {
                suppression_log = Some(msg);
            }
        }
        let msg = suppression_log.expect("suppression is logged");
        assert!(
            !msg.contains("+15551230000") && !msg.contains("5551230000"),
            "recipient must be masked; got: {msg}"
        );
        assert!(msg.contains("…0000"), "masked tail expected; got: {msg}");
    }

    #[test]
    fn reply_route_classifies_every_target_shape() {
        assert!(matches!(reply_route("noreply"), ReplyRoute::Noreply));
        assert!(matches!(reply_route("call:c7"), ReplyRoute::Call("c7")));
        assert!(matches!(
            reply_route("consult:42"),
            ReplyRoute::Consult("42")
        ));
        assert!(matches!(
            reply_route("email:a@b.com"),
            ReplyRoute::Email("a@b.com")
        ));
        assert!(matches!(
            reply_route("sms:conv-1"),
            ReplyRoute::Sms("conv-1")
        ));
        assert!(matches!(
            reply_route("smsto:+15551230000"),
            ReplyRoute::SmsTo("+15551230000")
        ));
        assert!(matches!(
            reply_route("imessage:ic-2"),
            ReplyRoute::Imessage("ic-2")
        ));
        // A bare string with no tag falls back to SMS.
        assert!(matches!(
            reply_route("+15551230000"),
            ReplyRoute::Sms("+15551230000")
        ));
        // A tagged-but-unrecognized mode is reported, not silently treated as SMS.
        assert!(matches!(
            reply_route("slack:xyz"),
            ReplyRoute::Unknown("slack")
        ));
    }
}
