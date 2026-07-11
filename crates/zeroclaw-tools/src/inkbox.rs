//! Inkbox agent tools — proactive actions the model can call.
//!
//! The native [`crate::inkbox`](super) channel handles *inbound* email / SMS /
//! iMessage / voice and lets the agent *reply*. These tools add the *outbound*
//! surface: send on any channel, place a call, and triage conversations —
//! without waiting for an inbound message.
//!
//! Each tool acts as one configured Inkbox identity (`[channels.inkbox.<alias>]`).
//! The `inkbox` SDK is blocking, so every call runs on the blocking pool via
//! [`InkboxCtx::run`]; the `AgentIdentity` facade is `!Send`, so it is resolved
//! and used entirely inside that closure.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use inkbox::contacts::resources::contacts::{
    CreateContactParams, ListContactsParams, UpdateContactParams,
};
use inkbox::contacts::types::{ContactEmail, ContactPhone};
use inkbox::phone::resources::texts::TextRecipients;
use inkbox::phone::types::CallOrigin;
use inkbox::{AgentIdentity, Inkbox, InkboxError};
use parking_lot::Mutex;
use serde_json::{Value, json};
use zeroclaw_api::attribution::ToolKind;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::tool_attribution;

/// Shared per-identity context: one Inkbox client and the identity handle every
/// tool acts as.
struct InkboxCtx {
    client: Arc<Inkbox>,
    identity: String,
}

impl InkboxCtx {
    /// Resolve the identity and run a blocking SDK closure on the blocking pool,
    /// rendering its JSON value as the tool output (or the error as a failure).
    async fn run<F>(&self, f: F) -> ToolResult
    where
        // `anyhow::Result` keeps the `Err` variant small (a thin boxed pointer);
        // `?` still converts the SDK's large `InkboxError` automatically.
        F: FnOnce(&AgentIdentity) -> anyhow::Result<Value> + Send + 'static,
    {
        let client = self.client.clone();
        let handle = self.identity.clone();
        let joined = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
            let identity = client.get_identity(&handle)?;
            f(&identity)
        })
        .await;
        match joined {
            Ok(Ok(value)) => ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()).into(),
                error: None,
            },
            Ok(Err(e)) => fail(e.to_string()),
            Err(e) => fail(format!("inkbox tool task failed: {e}")),
        }
    }

    /// Like [`Self::run`], but hands the closure the org-level client directly
    /// (no identity resolution) — for org-scoped resources like contacts.
    async fn run_client<F>(&self, f: F) -> ToolResult
    where
        F: FnOnce(&Inkbox) -> anyhow::Result<Value> + Send + 'static,
    {
        let client = self.client.clone();
        let joined =
            tokio::task::spawn_blocking(move || -> anyhow::Result<Value> { f(&client) }).await;
        match joined {
            Ok(Ok(value)) => ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()).into(),
                error: None,
            },
            Ok(Err(e)) => fail(e.to_string()),
            Err(e) => fail(format!("inkbox tool task failed: {e}")),
        }
    }
}

// ── arg helpers ──────────────────────────────────────────────────────────

/// Non-empty string arg, or `None`.
fn str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// String-or-array arg flattened to a `Vec<String>` (drops empties).
fn str_list(args: &Value, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().filter(|s| !s.is_empty()).map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// `str_list` as `Option`, `None` when empty.
fn opt_list(args: &Value, key: &str) -> Option<Vec<String>> {
    let v = str_list(args, key);
    (!v.is_empty()).then_some(v)
}

fn int_arg(args: &Value, key: &str, default: i64) -> i64 {
    args.get(key).and_then(Value::as_i64).unwrap_or(default)
}

fn bool_arg(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

/// Parse the `to` arg (E.164 string or list) into SMS recipients.
fn text_recipients(args: &Value) -> Option<TextRecipients> {
    match args.get("to") {
        Some(Value::String(s)) if !s.is_empty() => Some(TextRecipients::One(s.clone())),
        Some(Value::Array(_)) => {
            let many = str_list(args, "to");
            (!many.is_empty()).then_some(TextRecipients::Many(many))
        }
        _ => None,
    }
}

/// A validation/usage failure surfaced to the model (not a transport error).
fn fail(msg: impl Into<String>) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new().into(),
        error: Some(msg.into()),
    }
}

/// Outbound-call context (why we're calling + an optional opening line) handed
/// from `inkbox_place_call` to the voice bridge. Both run in the same daemon
/// process, so it's passed through an in-process registry keyed by a single-use
/// token (round-tripped via the call WS URL as `?context_token=`) — no temp
/// file. Mirrors the channel's `CALL_SINKS` / `CONSULT_SINKS` registries.
#[derive(Clone, Default)]
pub struct CallContext {
    /// Why we're calling, surfaced to the realtime model.
    pub purpose: Option<String>,
    /// Opening line to say verbatim as the first turn, when set.
    pub opening_message: Option<String>,
    /// The number we dialed, so the bridge can resolve that party's contact
    /// card (outbound legs carry no `call_id` to look the call record up by).
    pub remote_number: Option<String>,
}

static CALL_CONTEXTS: OnceLock<Mutex<HashMap<String, CallContext>>> = OnceLock::new();

fn call_contexts() -> &'static Mutex<HashMap<String, CallContext>> {
    CALL_CONTEXTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Stash single-use outbound-call context, returning the token to append to the
/// call WS URL as `context_token`. The voice bridge reclaims it via
/// [`take_call_context`] when Inkbox connects the audio leg.
fn stash_call_context(
    purpose: Option<&str>,
    opening: Option<&str>,
    remote_number: Option<&str>,
) -> String {
    let token = uuid::Uuid::new_v4().to_string();
    let ctx = CallContext {
        purpose: purpose.map(str::to_string).filter(|s| !s.is_empty()),
        opening_message: opening.map(str::to_string).filter(|s| !s.is_empty()),
        remote_number: remote_number.map(str::to_string).filter(|s| !s.is_empty()),
    };
    call_contexts().lock().insert(token.clone(), ctx);
    token
}

/// Take (and remove) the outbound-call context for `token`. Called by the
/// channel's voice handler on WS connect. `None` if the token is unknown
/// (already taken, or the daemon restarted between place-call and connect).
pub fn take_call_context(token: &str) -> Option<CallContext> {
    call_contexts().lock().remove(token)
}

// ── tools ────────────────────────────────────────────────────────────────

/// `inkbox_whoami` — report the configured identity's channels.
pub(crate) struct InkboxWhoami {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxWhoami {
    fn name(&self) -> &str {
        "inkbox_whoami"
    }
    fn description(&self) -> &str {
        "Return the configured Inkbox identity: handle, mailbox, phone number, \
         iMessage status, and tunnel host."
    }
    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
        Ok(self
            .ctx
            .run(|id| {
                Ok(json!({
                    "agent_handle": id.agent_handle(),
                    "display_name": id.display_name(),
                    "email_address": id.email_address(),
                    "phone_number": id.phone_number().map(|p| p.number),
                    "imessage_enabled": id.imessage_enabled(),
                    "tunnel_public_host": id.tunnel().map(|t| t.public_host),
                }))
            })
            .await)
    }
}
tool_attribution!(InkboxWhoami, ToolKind::Plugin);

/// `inkbox_send_email` — send mail from the identity's mailbox.
pub(crate) struct InkboxSendEmail {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxSendEmail {
    fn name(&self) -> &str {
        "inkbox_send_email"
    }
    fn description(&self) -> &str {
        "Send an email from the agent's Inkbox mailbox."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to": { "type": "array", "items": { "type": "string" }, "description": "Recipient email addresses." },
                "subject": { "type": "string" },
                "body_text": { "type": "string", "description": "Plain-text body." },
                "cc": { "type": "array", "items": { "type": "string" } },
                "bcc": { "type": "array", "items": { "type": "string" } },
                "in_reply_to_message_id": { "type": "string", "description": "RFC 5322 Message-ID to thread a reply." }
            },
            "required": ["to", "subject"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let to = str_list(&args, "to");
        if to.is_empty() {
            return Ok(fail("`to` must include at least one recipient"));
        }
        let subject = str_arg(&args, "subject").unwrap_or_else(|| "(no subject)".to_string());
        let body = str_arg(&args, "body_text");
        let cc = opt_list(&args, "cc");
        let bcc = opt_list(&args, "bcc");
        let in_reply_to = str_arg(&args, "in_reply_to_message_id");
        Ok(self
            .ctx
            .run(move |id| {
                let msg = id.send_email(
                    &to,
                    &subject,
                    body.as_deref(),
                    None,
                    cc.as_deref(),
                    bcc.as_deref(),
                    in_reply_to.as_deref(),
                    None,
                )?;
                Ok(serde_json::to_value(msg)?)
            })
            .await)
    }
}
tool_attribution!(InkboxSendEmail, ToolKind::Plugin);

/// `inkbox_send_sms` — send an SMS/MMS from the identity's phone number.
pub(crate) struct InkboxSendSms {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxSendSms {
    fn name(&self) -> &str {
        "inkbox_send_sms"
    }
    fn description(&self) -> &str {
        "Send a text from the agent's Inkbox number. Provide `conversation_id` to \
         reply into an existing thread, or `to` (one E.164 number, or a list for a \
         group MMS)."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "Message body." },
                "to": { "description": "One E.164 recipient or a list (group MMS). Mutually exclusive with conversation_id.",
                        "oneOf": [{ "type": "string" }, { "type": "array", "items": { "type": "string" } }] },
                "conversation_id": { "type": "string", "description": "Existing conversation UUID to reply into." },
                "media_urls": { "type": "array", "items": { "type": "string" }, "description": "Optional MMS media URLs." }
            },
            "required": ["text"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let Some(text) = str_arg(&args, "text") else {
            return Ok(fail("`text` is required"));
        };
        let conversation_id = str_arg(&args, "conversation_id");
        let recipients = text_recipients(&args);
        if recipients.is_some() == conversation_id.is_some() {
            return Ok(fail("specify exactly one of `to` or `conversation_id`"));
        }
        let media = opt_list(&args, "media_urls");
        Ok(self
            .ctx
            .run(move |id| {
                let msg = id.send_text(
                    recipients,
                    conversation_id.as_deref(),
                    Some(&text),
                    media.as_deref(),
                )?;
                Ok(serde_json::to_value(msg)?)
            })
            .await)
    }
}
tool_attribution!(InkboxSendSms, ToolKind::Plugin);

/// `inkbox_send_imessage` — send an iMessage from the identity.
pub(crate) struct InkboxSendImessage {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxSendImessage {
    fn name(&self) -> &str {
        "inkbox_send_imessage"
    }
    fn description(&self) -> &str {
        "Send an iMessage from the agent. Reply into a known `conversation_id`, or \
         `to` an E.164 number that has already connected to this agent over iMessage."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" },
                "to": { "type": "string", "description": "E.164 recipient. Mutually exclusive with conversation_id." },
                "conversation_id": { "type": "string", "description": "Existing iMessage conversation UUID." },
                "media_urls": { "type": "array", "items": { "type": "string" }, "description": "At most one media URL." }
            }
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let text = str_arg(&args, "text");
        let media = opt_list(&args, "media_urls");
        if text.is_none() && media.is_none() {
            return Ok(fail("provide `text`, `media_urls`, or both"));
        }
        let to = str_arg(&args, "to");
        let conversation_id = str_arg(&args, "conversation_id");
        if to.is_some() == conversation_id.is_some() {
            return Ok(fail("specify exactly one of `to` or `conversation_id`"));
        }
        Ok(self
            .ctx
            .run(move |id| {
                let cid = match conversation_id {
                    Some(c) => Some(uuid::Uuid::parse_str(&c).map_err(|e| {
                        InkboxError::InvalidArgument(format!("invalid conversation_id {c:?}: {e}"))
                    })?),
                    None => None,
                };
                let msg = id.send_imessage(
                    to.as_deref(),
                    cid.as_ref(),
                    text.as_deref(),
                    media.as_deref(),
                    None,
                )?;
                Ok(serde_json::to_value(msg)?)
            })
            .await)
    }
}
tool_attribution!(InkboxSendImessage, ToolKind::Plugin);

/// `inkbox_place_call` — place an outbound call, bridged to the agent's voice WS.
pub(crate) struct InkboxPlaceCall {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxPlaceCall {
    fn name(&self) -> &str {
        "inkbox_place_call"
    }
    fn description(&self) -> &str {
        "Place an outbound call from the agent's Inkbox number. The call's audio \
         bridges to the agent over the tunnel's call-media WebSocket so the agent \
         speaks the conversation live."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to_number": { "type": "string", "description": "Recipient E.164 number." },
                "purpose": { "type": "string", "description": "Why you're calling — loaded into the live call so the agent opens with the right context." },
                "opening_message": { "type": "string", "description": "Optional exact opening line to say first when the callee picks up." },
                "client_websocket_url": { "type": "string", "description": "Optional explicit call-media WS URL; defaults to the agent's tunnel." }
            },
            "required": ["to_number"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let Some(to_number) = str_arg(&args, "to_number") else {
            return Ok(fail("`to_number` is required"));
        };
        let explicit_ws = str_arg(&args, "client_websocket_url");
        let purpose = str_arg(&args, "purpose");
        let opening = str_arg(&args, "opening_message");
        Ok(self
            .ctx
            .run(move |id| {
                // Default the media leg to this identity's tunnel so the call
                // bridges through the channel's `/phone/media/ws` handler.
                let mut ws = explicit_ws.or_else(|| {
                    id.tunnel()
                        .map(|t| t.public_host)
                        .map(|host| format!("wss://{host}/phone/media/ws"))
                });
                // Port call context to the realtime bridge via a single-use
                // token the voice handler reads on connect. Always stash on
                // outbound (even with no purpose/opening) so the bridge can
                // resolve the dialed party's contact card.
                if ws.is_some() {
                    let token = stash_call_context(
                        purpose.as_deref(),
                        opening.as_deref(),
                        Some(&to_number),
                    );
                    ws = ws.map(|u| {
                        let sep = if u.contains('?') { '&' } else { '?' };
                        format!("{u}{sep}context_token={token}")
                    });
                }
                // Ride this identity's dedicated number; shared-iMessage-line
                // origination is wired separately.
                let call = id.place_call(&to_number, CallOrigin::DedicatedNumber, ws.as_deref())?;
                Ok(serde_json::to_value(call)?)
            })
            .await)
    }
}
tool_attribution!(InkboxPlaceCall, ToolKind::Plugin);

/// `inkbox_list_text_conversations` — triage SMS/MMS threads.
pub(crate) struct InkboxListTextConversations {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxListTextConversations {
    fn name(&self) -> &str {
        "inkbox_list_text_conversations"
    }
    fn description(&self) -> &str {
        "List the agent's text conversation summaries (newest first), returning \
         conversation IDs to reply into."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "default": 25 },
                "offset": { "type": "integer", "default": 0 },
                "include_groups": { "type": "boolean", "default": true }
            }
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let limit = int_arg(&args, "limit", 25);
        let offset = int_arg(&args, "offset", 0);
        let include_groups = bool_arg(&args, "include_groups", true);
        Ok(self
            .ctx
            .run(move |id| {
                let convos = id.list_text_conversations(limit, offset, None, include_groups)?;
                Ok(serde_json::to_value(convos)?)
            })
            .await)
    }
}
tool_attribution!(InkboxListTextConversations, ToolKind::Plugin);

/// `inkbox_list_imessage_conversations` — triage iMessage threads.
pub(crate) struct InkboxListImessageConversations {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxListImessageConversations {
    fn name(&self) -> &str {
        "inkbox_list_imessage_conversations"
    }
    fn description(&self) -> &str {
        "List the agent's iMessage conversation summaries (newest first), with \
         conversation IDs and assignment status."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "default": 25 },
                "offset": { "type": "integer", "default": 0 }
            }
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let limit = int_arg(&args, "limit", 25);
        let offset = int_arg(&args, "offset", 0);
        Ok(self
            .ctx
            .run(move |id| {
                let convos = id.list_imessage_conversations(limit, offset, None)?;
                Ok(serde_json::to_value(convos)?)
            })
            .await)
    }
}
tool_attribution!(InkboxListImessageConversations, ToolKind::Plugin);

/// `inkbox_get_text_conversation` — read an SMS/MMS thread.
pub(crate) struct InkboxGetTextConversation {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxGetTextConversation {
    fn name(&self) -> &str {
        "inkbox_get_text_conversation"
    }
    fn description(&self) -> &str {
        "Read messages in one text conversation (newest first). `conversation` is \
         the conversation UUID or the remote E.164 number."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "conversation": { "type": "string", "description": "Conversation UUID or remote E.164 number." },
                "limit": { "type": "integer", "default": 50 },
                "offset": { "type": "integer", "default": 0 }
            },
            "required": ["conversation"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let Some(conversation) = str_arg(&args, "conversation") else {
            return Ok(fail("`conversation` is required"));
        };
        let limit = int_arg(&args, "limit", 50);
        let offset = int_arg(&args, "offset", 0);
        Ok(self
            .ctx
            .run(move |id| {
                let msgs = id.get_text_conversation(&conversation, limit, offset)?;
                Ok(serde_json::to_value(msgs)?)
            })
            .await)
    }
}
tool_attribution!(InkboxGetTextConversation, ToolKind::Plugin);

/// `inkbox_get_imessage_conversation` — read an iMessage thread.
pub(crate) struct InkboxGetImessageConversation {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxGetImessageConversation {
    fn name(&self) -> &str {
        "inkbox_get_imessage_conversation"
    }
    fn description(&self) -> &str {
        "Read one iMessage conversation by its UUID, including any tapback reactions."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "conversation_id": { "type": "string", "description": "iMessage conversation UUID." }
            },
            "required": ["conversation_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let Some(cid) = str_arg(&args, "conversation_id") else {
            return Ok(fail("`conversation_id` is required"));
        };
        Ok(self
            .ctx
            .run(move |id| {
                let uuid = uuid::Uuid::parse_str(&cid).map_err(|e| {
                    InkboxError::InvalidArgument(format!("invalid conversation_id {cid:?}: {e}"))
                })?;
                let convo = id.get_imessage_conversation(&uuid)?;
                Ok(serde_json::to_value(convo)?)
            })
            .await)
    }
}
tool_attribution!(InkboxGetImessageConversation, ToolKind::Plugin);

/// `inkbox_list_emails` — list inbox messages for triage.
pub(crate) struct InkboxListEmails {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxListEmails {
    fn name(&self) -> &str {
        "inkbox_list_emails"
    }
    fn description(&self) -> &str {
        "List the agent's emails (newest first) for triage. Use inkbox_get_email \
         for the full body of a specific message."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "default": 25, "description": "Messages per page (1-100)." }
            }
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let limit = int_arg(&args, "limit", 25);
        Ok(self
            .ctx
            .run(move |id| {
                let msgs = id.iter_emails(Some(limit), None)?;
                Ok(serde_json::to_value(msgs)?)
            })
            .await)
    }
}
tool_attribution!(InkboxListEmails, ToolKind::Plugin);

/// `inkbox_get_email` — read one email with its full body.
pub(crate) struct InkboxGetEmail {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxGetEmail {
    fn name(&self) -> &str {
        "inkbox_get_email"
    }
    fn description(&self) -> &str {
        "Fetch a single email by its message UUID, including the full body."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "message_id": { "type": "string", "description": "Inkbox message UUID (the `id` field, not the RFC Message-ID)." }
            },
            "required": ["message_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let Some(mid) = str_arg(&args, "message_id") else {
            return Ok(fail("`message_id` is required"));
        };
        Ok(self
            .ctx
            .run(move |id| {
                let detail = id.get_message(&mid)?;
                Ok(serde_json::to_value(detail)?)
            })
            .await)
    }
}
tool_attribution!(InkboxGetEmail, ToolKind::Plugin);

/// `inkbox_lookup_contact` — find a contact by email or phone.
pub(crate) struct InkboxLookupContact {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxLookupContact {
    fn name(&self) -> &str {
        "inkbox_lookup_contact"
    }
    fn description(&self) -> &str {
        "Look up a contact in the org address book by email or phone (E.164). \
         Provide exactly one. Useful to resolve who a sender is."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "email": { "type": "string" },
                "phone": { "type": "string", "description": "E.164 number." }
            }
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let email = str_arg(&args, "email");
        let phone = str_arg(&args, "phone");
        if email.is_some() == phone.is_some() {
            return Ok(fail("provide exactly one of `email` or `phone`"));
        }
        Ok(self
            .ctx
            .run_client(move |c| {
                let found =
                    c.contacts()
                        .lookup(email.as_deref(), None, None, phone.as_deref(), None)?;
                Ok(serde_json::to_value(found)?)
            })
            .await)
    }
}
tool_attribution!(InkboxLookupContact, ToolKind::Plugin);

/// `inkbox_list_contacts` — list/search the org address book.
pub(crate) struct InkboxListContacts {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxListContacts {
    fn name(&self) -> &str {
        "inkbox_list_contacts"
    }
    fn description(&self) -> &str {
        "List or search contacts in the org address book. `q` is a case-insensitive \
         substring filter across names, company, job title, and notes."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "q": { "type": "string", "description": "Search filter." },
                "limit": { "type": "integer", "default": 25 },
                "offset": { "type": "integer", "default": 0 }
            }
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let params = ListContactsParams {
            q: str_arg(&args, "q"),
            order: None,
            limit: Some(int_arg(&args, "limit", 25)),
            offset: Some(int_arg(&args, "offset", 0)),
        };
        Ok(self
            .ctx
            .run_client(move |c| Ok(serde_json::to_value(c.contacts().list(&params)?)?))
            .await)
    }
}
tool_attribution!(InkboxListContacts, ToolKind::Plugin);

/// `inkbox_create_contact` — add a contact to the org address book.
pub(crate) struct InkboxCreateContact {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxCreateContact {
    fn name(&self) -> &str {
        "inkbox_create_contact"
    }
    fn description(&self) -> &str {
        "Create a contact in the org address book."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "given_name": { "type": "string" },
                "family_name": { "type": "string" },
                "company_name": { "type": "string" },
                "job_title": { "type": "string" },
                "notes": { "type": "string" },
                "emails": { "type": "array", "items": { "type": "string" } },
                "phones": { "type": "array", "items": { "type": "string" }, "description": "E.164 numbers." }
            }
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let emails = opt_list(&args, "emails").map(|v| {
            v.into_iter()
                .map(|value| ContactEmail {
                    label: None,
                    value,
                    is_primary: false,
                })
                .collect::<Vec<_>>()
        });
        let phones = opt_list(&args, "phones").map(|v| {
            v.into_iter()
                .map(|value| ContactPhone {
                    label: None,
                    value,
                    is_primary: false,
                })
                .collect::<Vec<_>>()
        });
        let params = CreateContactParams {
            given_name: str_arg(&args, "given_name"),
            family_name: str_arg(&args, "family_name"),
            company_name: str_arg(&args, "company_name"),
            job_title: str_arg(&args, "job_title"),
            notes: str_arg(&args, "notes"),
            emails,
            phones,
            ..Default::default()
        };
        Ok(self
            .ctx
            .run_client(move |c| Ok(serde_json::to_value(c.contacts().create(&params)?)?))
            .await)
    }
}
tool_attribution!(InkboxCreateContact, ToolKind::Plugin);

/// `inkbox_update_contact` — edit fields on an existing contact.
pub(crate) struct InkboxUpdateContact {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxUpdateContact {
    fn name(&self) -> &str {
        "inkbox_update_contact"
    }
    fn description(&self) -> &str {
        "Update fields on a contact by id. Only provided fields change."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "contact_id": { "type": "string" },
                "given_name": { "type": "string" },
                "family_name": { "type": "string" },
                "company_name": { "type": "string" },
                "job_title": { "type": "string" },
                "notes": { "type": "string" }
            },
            "required": ["contact_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let Some(id) = str_arg(&args, "contact_id") else {
            return Ok(fail("`contact_id` is required"));
        };
        // `Option<Option<T>>`: outer None omits (unchanged), `Some(Some(v))` sets it.
        let params = UpdateContactParams {
            given_name: str_arg(&args, "given_name").map(Some),
            family_name: str_arg(&args, "family_name").map(Some),
            company_name: str_arg(&args, "company_name").map(Some),
            job_title: str_arg(&args, "job_title").map(Some),
            notes: str_arg(&args, "notes").map(Some),
            ..Default::default()
        };
        Ok(self
            .ctx
            .run_client(move |c| Ok(serde_json::to_value(c.contacts().update(&id, &params)?)?))
            .await)
    }
}
tool_attribution!(InkboxUpdateContact, ToolKind::Plugin);

/// `inkbox_delete_contact` — remove a contact by id.
pub(crate) struct InkboxDeleteContact {
    ctx: Arc<InkboxCtx>,
}

#[async_trait]
impl Tool for InkboxDeleteContact {
    fn name(&self) -> &str {
        "inkbox_delete_contact"
    }
    fn description(&self) -> &str {
        "Delete a contact from the org address book by id."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "contact_id": { "type": "string" } },
            "required": ["contact_id"]
        })
    }
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let Some(id) = str_arg(&args, "contact_id") else {
            return Ok(fail("`contact_id` is required"));
        };
        Ok(self
            .ctx
            .run_client(move |c| {
                c.contacts().delete(&id)?;
                Ok(json!({ "deleted": id }))
            })
            .await)
    }
}
tool_attribution!(InkboxDeleteContact, ToolKind::Plugin);

/// Build the Inkbox tool set for one configured identity. Returns an empty
/// vector when the client can't be constructed (bad base URL / key) — the caller
/// simply registers no Inkbox tools in that case.
///
/// # Arguments
/// * `api_key` - the identity's Inkbox API key.
/// * `identity` - the agent identity handle to act as.
/// * `base_url` - API base URL (e.g. `https://inkbox.ai`).
pub fn build_inkbox_tools(api_key: &str, identity: &str, base_url: &str) -> Vec<Arc<dyn Tool>> {
    // `reqwest::blocking::Client::build` spins up and drops a temporary tokio
    // runtime internally; on a tokio worker thread that drop panics with "cannot
    // drop a runtime in an async context". Build off-runtime on a plain OS
    // thread (the resulting `Arc<Inkbox>` is `Send`).
    let (key, base) = (api_key.to_string(), base_url.to_string());
    let built = std::thread::spawn(move || Inkbox::builder(key).base_url(base).build()).join();
    let client = match built {
        Ok(Ok(client)) => client,
        // A build failure means this identity gets ZERO tools — log it so the
        // operator can see why (bad base URL / key) instead of silent absence.
        Ok(Err(e)) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                format!(
                    "[inkbox] could not build client for identity {identity:?}; no tools registered: {e}"
                ),
            );
            return Vec::new();
        }
        Err(_) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                format!(
                    "[inkbox] client-build thread panicked for identity {identity:?}; no tools registered"
                ),
            );
            return Vec::new();
        }
    };
    let ctx = Arc::new(InkboxCtx {
        client,
        identity: identity.to_string(),
    });
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(InkboxWhoami { ctx: ctx.clone() }),
        Arc::new(InkboxSendEmail { ctx: ctx.clone() }),
        Arc::new(InkboxSendSms { ctx: ctx.clone() }),
        Arc::new(InkboxSendImessage { ctx: ctx.clone() }),
        Arc::new(InkboxPlaceCall { ctx: ctx.clone() }),
        Arc::new(InkboxListTextConversations { ctx: ctx.clone() }),
        Arc::new(InkboxListImessageConversations { ctx: ctx.clone() }),
        Arc::new(InkboxGetTextConversation { ctx: ctx.clone() }),
        Arc::new(InkboxGetImessageConversation { ctx: ctx.clone() }),
        Arc::new(InkboxListEmails { ctx: ctx.clone() }),
        Arc::new(InkboxGetEmail { ctx: ctx.clone() }),
        Arc::new(InkboxLookupContact { ctx: ctx.clone() }),
        Arc::new(InkboxListContacts { ctx: ctx.clone() }),
        Arc::new(InkboxCreateContact { ctx: ctx.clone() }),
        Arc::new(InkboxUpdateContact { ctx: ctx.clone() }),
        Arc::new(InkboxDeleteContact { ctx }),
    ];
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_arg_filters_empty_and_non_strings() {
        let v = json!({ "a": "x", "b": "", "c": 3 });
        assert_eq!(str_arg(&v, "a").as_deref(), Some("x"));
        assert_eq!(str_arg(&v, "b"), None);
        assert_eq!(str_arg(&v, "c"), None);
        assert_eq!(str_arg(&v, "missing"), None);
    }

    #[test]
    fn str_list_handles_string_array_and_drops_empties() {
        assert_eq!(
            str_list(&json!({ "k": "one" }), "k"),
            vec!["one".to_string()]
        );
        assert_eq!(
            str_list(&json!({ "k": ["a", "", "b"] }), "k"),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(str_list(&json!({ "k": "" }), "k").is_empty());
        assert!(str_list(&json!({}), "k").is_empty());
    }

    #[test]
    fn opt_list_is_none_when_empty() {
        assert_eq!(
            opt_list(&json!({ "k": ["a"] }), "k"),
            Some(vec!["a".to_string()])
        );
        assert_eq!(opt_list(&json!({ "k": [] }), "k"), None);
    }

    #[test]
    fn int_and_bool_args_fall_back_to_defaults() {
        assert_eq!(int_arg(&json!({ "n": 5 }), "n", 1), 5);
        assert_eq!(int_arg(&json!({}), "n", 1), 1);
        assert!(bool_arg(&json!({ "b": true }), "b", false));
        assert!(!bool_arg(&json!({}), "b", false));
    }

    #[test]
    fn text_recipients_parses_one_and_many() {
        assert!(matches!(
            text_recipients(&json!({ "to": "+15551230000" })),
            Some(TextRecipients::One(_))
        ));
        assert!(matches!(
            text_recipients(&json!({ "to": ["+1", "+2"] })),
            Some(TextRecipients::Many(_))
        ));
        assert!(text_recipients(&json!({ "to": "" })).is_none());
        assert!(text_recipients(&json!({})).is_none());
    }

    #[test]
    fn call_context_round_trips_in_memory_and_is_single_use() {
        let token = stash_call_context(Some("confirm order"), None, Some("+15551234567"));
        let ctx = take_call_context(&token).expect("context present");
        assert_eq!(ctx.purpose.as_deref(), Some("confirm order"));
        assert_eq!(ctx.opening_message, None);
        assert_eq!(ctx.remote_number.as_deref(), Some("+15551234567"));
        // Single-use: a second take finds nothing.
        assert!(take_call_context(&token).is_none());
        // Empty strings are normalized to None.
        let t2 = stash_call_context(Some(""), Some("hi"), Some(""));
        let c2 = take_call_context(&t2).unwrap();
        assert_eq!(c2.purpose, None);
        assert_eq!(c2.opening_message.as_deref(), Some("hi"));
        assert_eq!(c2.remote_number, None);
    }
}
