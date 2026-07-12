//! Delivery-failure retry loop: wake the agent to fix and resend undelivered
//! outbound messages.
//!
//! An outbound message can die two ways, and without this loop the agent never
//! learns about either: **rejected at send time** (the API call errors back
//! with a content-policy block, an opted-out recipient, a bad address, an
//! over-long body), or **failed after acceptance** (the carrier flags it or
//! the receiving mail server bounces it, reported later via
//! `text.delivery_failed` / `imessage.delivery_failed` / `message.bounced` /
//! `message.failed` webhooks). Both surfaces funnel into one tracker: the
//! agent is woken in the same conversation with the exact error plus its own
//! undelivered body, and instructed to fix and resend (or reply `[SILENT]`).
//!
//! Guardrails: a hard cap of [`MAX_ATTEMPTS`] total sends per logical reply
//! (the budget is shared across both surfaces, keyed by conversation +
//! recipient and merged by max), reset on a fresh inbound, a delivered
//! receipt, or a [`STATE_TTL`] timeout; webhook replays are deduped per
//! failed message id; transient failures are excluded (the transport's own
//! retries handle those — waking the agent would double-send).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use inkbox::{ApiErrorDetail, InkboxError};
use serde_json::Value;
use tokio::sync::mpsc;
use zeroclaw_api::channel::ChannelMessage;

/// Hard cap: total sends per logical reply. Failures 1 and 2 wake the agent;
/// the failure that reaches the cap goes quiet with a loud log line.
const MAX_ATTEMPTS: u32 = 3;
/// Budget entries older than this no longer count toward the cap.
const STATE_TTL: Duration = Duration::from_secs(30 * 60);
/// How much of the undelivered body to echo back to the agent.
const BODY_SNIPPET_CHARS: usize = 400;
/// Webhook-replay dedup window per failed message id.
const DEDUP_TTL: Duration = Duration::from_secs(300);
/// Opportunistic prune threshold for the budget map.
const BUDGET_PRUNE_LEN: usize = 512;

/// One failure to report to the agent, normalized across both surfaces.
pub(super) struct FailureNote {
    /// `"sms"` / `"imessage"` / `"email"`.
    pub mode: &'static str,
    /// `"send_rejected"` / `"delivery_failed"` / `"bounced"`.
    pub stage: &'static str,
    /// Inkbox conversation id, when the failure names one.
    pub conversation_id: Option<String>,
    /// Remote number / email address, when the failure names one.
    pub target: Option<String>,
    /// Fallback budget key when neither conversation nor target resolved.
    pub chat_id: Option<String>,
    /// Tagged reply target the wake-up (and the corrected resend) routes to.
    pub reply_target: String,
    /// The undelivered body, echoed back so the agent can fix it.
    pub failed_body: String,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    /// Email threading: subject + message id of the failed mail, so the
    /// corrected resend stays on the same thread.
    pub subject: Option<String>,
    pub thread_message_id: Option<String>,
}

/// A classified send-time error: what to tell the agent, and whether the
/// failure is transient (transient ones are left to the transport's retries).
struct SendFailure {
    error_code: Option<String>,
    error_detail: String,
    retryable: bool,
}

struct BudgetEntry {
    attempts: u32,
    at: Instant,
}

/// Shared retry-budget + dedup state for one channel instance. The send path
/// and the inbound webhook server both hold it (via `Arc`), so synchronous
/// rejections and asynchronous delivery failures draw down one budget.
pub(super) struct FailureTracker {
    /// ZeroClaw channel alias, stamped onto synthetic wake-up messages.
    alias: String,
    budget: Mutex<HashMap<String, BudgetEntry>>,
    dedup: Mutex<HashMap<String, Instant>>,
    /// Last inbound sender label per reply target. Conversation history is
    /// sender-scoped, so a wake-up must reuse the label the inbound mapping
    /// stamped (the resolved contact name) to land in the same session.
    labels: Mutex<HashMap<String, String>>,
    /// Inbound sink, installed by `listen` (wakes are dropped until then).
    tx: Mutex<Option<mpsc::Sender<ChannelMessage>>>,
}

impl FailureTracker {
    pub(super) fn new(alias: impl Into<String>) -> Self {
        Self {
            alias: alias.into(),
            budget: Mutex::new(HashMap::new()),
            dedup: Mutex::new(HashMap::new()),
            labels: Mutex::new(HashMap::new()),
            tx: Mutex::new(None),
        }
    }

    /// Install the inbound sink wake-ups are enqueued into. Called by
    /// `listen`, which owns the orchestrator's sender.
    pub(super) fn set_sender(&self, tx: mpsc::Sender<ChannelMessage>) {
        *self.tx.lock().unwrap() = Some(tx);
    }

    /// Remember the sender label an inbound message carried, keyed by its
    /// reply target, so a later wake-up joins the same sender-scoped session.
    pub(super) fn remember_sender(&self, reply_target: &str, sender: &str) {
        if reply_target.is_empty() || sender.is_empty() {
            return;
        }
        let mut labels = self.labels.lock().unwrap();
        // Bound the map; losing a label only costs session continuity.
        if labels.len() > BUDGET_PRUNE_LEN {
            labels.clear();
        }
        labels.insert(reply_target.to_string(), sender.to_string());
    }

    /// Record one failure and return the merged attempt count. Reads the max
    /// live count across every derivable key, bumps it, and writes the result
    /// back under every key — so a failure keyed by conversation and a later
    /// one keyed by number still share one budget.
    fn record(&self, keys: &[String]) -> u32 {
        let mut store = self.budget.lock().unwrap();
        let now = Instant::now();
        let mut attempts = 0;
        for key in keys {
            if let Some(entry) = store.get(key)
                && now.duration_since(entry.at) <= STATE_TTL
            {
                attempts = attempts.max(entry.attempts);
            }
        }
        attempts += 1;
        for key in keys {
            store.insert(key.clone(), BudgetEntry { attempts, at: now });
        }
        // Opportunistic prune so an abandoned map can't grow unbounded.
        if store.len() > BUDGET_PRUNE_LEN {
            store.retain(|_, e| now.duration_since(e.at) <= STATE_TTL);
        }
        attempts
    }

    /// Reset the budget for a conversation/recipient — a fresh inbound or a
    /// delivered receipt proves the thread is healthy again. Clears the
    /// superset of keys (including the chat fallback) so a budget recorded
    /// under any derivation is wiped.
    pub(super) fn clear(
        &self,
        mode: &str,
        conversation_id: Option<&str>,
        target: Option<&str>,
        chat_id: Option<&str>,
    ) {
        let mut keys = budget_keys(mode, conversation_id, target, None);
        if let Some(chat) = chat_id.map(str::trim).filter(|c| !c.is_empty()) {
            keys.push(format!("{mode}:chat:{chat}"));
        }
        let mut store = self.budget.lock().unwrap();
        for key in &keys {
            store.remove(key);
        }
    }

    /// True if this failed-message id was already handled inside the dedup
    /// window (webhook replay); otherwise mark it handled.
    fn dedup_seen(&self, key: &str) -> bool {
        let mut seen = self.dedup.lock().unwrap();
        let now = Instant::now();
        seen.retain(|_, at| now.duration_since(*at) <= DEDUP_TTL);
        if seen.contains_key(key) {
            return true;
        }
        seen.insert(key.to_string(), now);
        false
    }

    /// The funnel both failure surfaces feed: draw down the budget and either
    /// wake the agent with the error + undelivered body, or go quiet at the cap.
    pub(super) fn note_failure(&self, note: FailureNote) {
        let keys = budget_keys(
            note.mode,
            note.conversation_id.as_deref(),
            note.target.as_deref(),
            note.chat_id.as_deref(),
        );
        if keys.is_empty() {
            // No key means no cap — never risk an unbounded wake loop.
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                format!(
                    "[inkbox] outbound {} failure had no conversation/target key; not waking agent",
                    note.mode
                ),
            );
            return;
        }
        let attempts = self.record(&keys);
        let who = note
            .target
            .as_deref()
            .or(note.conversation_id.as_deref())
            .unwrap_or(&note.reply_target);
        if attempts >= MAX_ATTEMPTS {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                format!(
                    "[inkbox] outbound {} to {} failed {attempts}/{MAX_ATTEMPTS} times ({} {}) — \
                     retry budget exhausted, thread goes quiet",
                    note.mode,
                    mask_target(who),
                    note.error_code.as_deref().unwrap_or(""),
                    truncate(note.error_detail.as_deref().unwrap_or(""), 120),
                ),
            );
            return;
        }
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            format!(
                "[inkbox] woke agent about failed outbound {} (attempt {attempts}/{MAX_ATTEMPTS}, \
                 stage={}, error={})",
                note.mode,
                note.stage,
                note.error_code.as_deref().unwrap_or(""),
            ),
        );

        let text = wake_text(&note, attempts);
        // Reuse the inbound sender label when known: conversation history is
        // sender-scoped, so this is what places the wake-up (and the agent's
        // corrected reply) in the conversation that failed.
        let sender_label = self
            .labels
            .lock()
            .unwrap()
            .get(&note.reply_target)
            .cloned()
            .unwrap_or_else(|| who.to_string());
        let id = note
            .thread_message_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("delivery-failure:{}:{attempts}", note.mode));
        let mut cm = ChannelMessage::new(
            id,
            sender_label,
            note.reply_target.clone(),
            text,
            "inkbox",
            super::now_secs(),
        );
        cm.channel_alias = Some(self.alias.clone());
        cm.subject = note.subject.clone();

        let tx = self.tx.lock().unwrap().clone();
        match tx {
            Some(tx) => {
                if let Err(e) = tx.try_send(cm) {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        format!("[inkbox] dropped delivery-failure wake-up on backpressure: {e}"),
                    );
                }
            }
            None => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "[inkbox] delivery-failure wake-up dropped — channel not listening yet",
                );
            }
        }
    }

    /// Send-time surface: classify a failed send and wake the agent when the
    /// failure is one it can fix (content policy, opt-out, bad address,
    /// too-long). Transient transport errors are left to the transport's own
    /// retries. `recipient` is the tagged reply target the send used.
    pub(super) fn note_send_rejection(&self, recipient: &str, content: &str, err: &anyhow::Error) {
        // Only Inkbox API errors are classifiable; anything else (task panic
        // context, local target-parse bugs) has nothing the agent can fix.
        let Some(inkbox_err) = err.downcast_ref::<InkboxError>() else {
            return;
        };
        let failure = classify_send_error(inkbox_err);
        if failure.retryable {
            return;
        }
        let (mode, conversation_id, target) = match super::reply_route(recipient) {
            super::ReplyRoute::Email(id) => ("email", None, Some(id.to_string())),
            super::ReplyRoute::Sms(id) => ("sms", Some(id.to_string()), None),
            super::ReplyRoute::SmsTo(id) => ("sms", None, Some(id.to_string())),
            super::ReplyRoute::Imessage(id) => ("imessage", Some(id.to_string()), None),
            _ => return,
        };
        self.note_failure(FailureNote {
            mode,
            stage: "send_rejected",
            conversation_id,
            target,
            chat_id: Some(recipient.to_string()),
            reply_target: recipient.to_string(),
            failed_body: content.to_string(),
            error_code: failure.error_code,
            error_detail: Some(failure.error_detail),
            subject: None,
            thread_message_id: None,
        });
    }

    /// Webhook surface: watch the event stream for outbound delivery failures
    /// (wake), delivered receipts and fresh inbounds (budget reset). Runs on
    /// every verified webhook, before the inbound mapping; events it doesn't
    /// recognize are ignored.
    pub(super) fn observe_event(&self, payload: &Value) {
        match payload.get("event_type").and_then(Value::as_str) {
            // Fresh inbound: the thread is alive — reset its budget.
            Some("text.received") => {
                let t = payload.pointer("/data/text_message");
                let conv = str_at(t, "conversation_id");
                let remote = str_at(t, "remote_phone_number");
                let chat = sms_reply_target(conv.as_deref(), remote.as_deref());
                self.clear("sms", conv.as_deref(), remote.as_deref(), chat.as_deref());
            }
            Some("imessage.received") => {
                let m = payload.pointer("/data/message");
                let conv = str_at(m, "conversation_id");
                let remote = str_at(m, "remote_number");
                let chat = conv.as_deref().map(|c| format!("imessage:{c}"));
                self.clear(
                    "imessage",
                    conv.as_deref(),
                    remote.as_deref(),
                    chat.as_deref(),
                );
            }
            Some("message.received") => {
                let m = payload.pointer("/data/message");
                let from = str_at(m, "from_address");
                let chat = from.as_deref().map(|f| format!("email:{f}"));
                self.clear("email", None, from.as_deref(), chat.as_deref());
            }
            // Delivered receipt: the last send landed — reset the budget.
            Some("text.delivered") => {
                let t = payload.pointer("/data/text_message");
                self.clear(
                    "sms",
                    str_at(t, "conversation_id").as_deref(),
                    str_at(t, "remote_phone_number").as_deref(),
                    None,
                );
            }
            Some("imessage.delivered") => {
                let m = payload.pointer("/data/message");
                self.clear(
                    "imessage",
                    str_at(m, "conversation_id").as_deref(),
                    str_at(m, "remote_number").as_deref(),
                    None,
                );
            }
            Some("text.delivery_failed") => self.on_text_delivery_failed(payload),
            Some("imessage.delivery_failed") => self.on_imessage_delivery_failed(payload),
            Some(ev @ ("message.bounced" | "message.failed")) => {
                self.on_mail_delivery_failure(payload, ev)
            }
            _ => {}
        }
    }

    fn on_text_delivery_failed(&self, payload: &Value) {
        let Some(t) = payload.pointer("/data/text_message") else {
            return;
        };
        // Only our own outbound can be undelivered; ignore inbound lifecycle.
        if str_at(Some(t), "direction").as_deref() != Some("outbound") {
            return;
        }
        let Some(id) = str_at(Some(t), "id") else {
            return;
        };
        if self.dedup_seen(&format!("textfail:{id}")) {
            return;
        }
        // Group MMS names the failed recipient at the data level; 1:1 puts the
        // counterparty on the message row.
        let remote = payload
            .pointer("/data/recipient_phone_number")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .or_else(|| str_at(Some(t), "remote_phone_number"));
        let conv = str_at(Some(t), "conversation_id");
        let Some(reply_target) = sms_reply_target(conv.as_deref(), remote.as_deref()) else {
            return;
        };
        // Message-level error fields, falling back to the matching recipient
        // row (group sends report per-recipient).
        let mut error_code = str_at(Some(t), "error_code");
        let mut error_detail = str_at(Some(t), "error_detail");
        if error_code.is_none()
            && let (Some(remote), Some(rows)) = (
                remote.as_deref(),
                t.get("recipients").and_then(Value::as_array),
            )
        {
            let want = digits(remote);
            for row in rows {
                let row_number = str_at(Some(row), "recipient_phone_number").unwrap_or_default();
                if digits(&row_number) == want {
                    error_code = str_at(Some(row), "error_code");
                    error_detail = error_detail.or_else(|| str_at(Some(row), "error_detail"));
                    break;
                }
            }
        }
        self.note_failure(FailureNote {
            mode: "sms",
            stage: "delivery_failed",
            conversation_id: conv,
            target: remote,
            chat_id: None,
            reply_target,
            failed_body: str_at(Some(t), "text").unwrap_or_default(),
            error_code,
            error_detail,
            subject: None,
            thread_message_id: None,
        });
    }

    fn on_imessage_delivery_failed(&self, payload: &Value) {
        let Some(m) = payload.pointer("/data/message") else {
            return;
        };
        if str_at(Some(m), "direction").as_deref() == Some("inbound") {
            return;
        }
        let Some(id) = str_at(Some(m), "id") else {
            return;
        };
        if self.dedup_seen(&format!("imessagefail:{id}")) {
            return;
        }
        // A shared-line iMessage reply must target the conversation row.
        let Some(conv) = str_at(Some(m), "conversation_id") else {
            return;
        };
        self.note_failure(FailureNote {
            mode: "imessage",
            stage: "delivery_failed",
            conversation_id: Some(conv.clone()),
            target: str_at(Some(m), "remote_number"),
            chat_id: None,
            reply_target: format!("imessage:{conv}"),
            failed_body: str_at(Some(m), "content")
                .or_else(|| str_at(Some(m), "text"))
                .unwrap_or_default(),
            error_code: str_at(Some(m), "error_code"),
            error_detail: str_at(Some(m), "error_detail")
                .or_else(|| str_at(Some(m), "error_message")),
            subject: None,
            thread_message_id: None,
        });
    }

    fn on_mail_delivery_failure(&self, payload: &Value, event_type: &str) {
        let Some(m) = payload.pointer("/data/message") else {
            return;
        };
        // Bounces only make sense for our own outbound; skip when the wire
        // says otherwise (absent direction is treated as outbound).
        if let Some(direction) = str_at(Some(m), "direction")
            && direction != "outbound"
        {
            return;
        }
        let Some(id) = str_at(Some(m), "id") else {
            return;
        };
        if self.dedup_seen(&format!("mailfail:{id}")) {
            return;
        }
        let to_address = m
            .get("to_addresses")
            .and_then(Value::as_array)
            .and_then(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .find(|s| !s.is_empty())
            })
            .map(str::to_string);
        let Some(to_address) = to_address else {
            return;
        };
        let subject = str_at(Some(m), "subject");
        let status = str_at(Some(m), "status")
            .unwrap_or_else(|| event_type.trim_start_matches("message.").to_string());
        let subject_part = subject
            .as_deref()
            .map(|s| format!(" (subject {s:?})"))
            .unwrap_or_default();
        self.note_failure(FailureNote {
            mode: "email",
            stage: if event_type == "message.bounced" {
                "bounced"
            } else {
                "delivery_failed"
            },
            conversation_id: None,
            target: Some(to_address.clone()),
            chat_id: None,
            reply_target: format!("email:{to_address}"),
            failed_body: str_at(Some(m), "snippet")
                .or_else(|| str_at(Some(m), "body"))
                .unwrap_or_default(),
            error_code: Some(status.clone()),
            error_detail: Some(format!(
                "The email to {to_address}{subject_part} was returned as {status} by the \
                 receiving server."
            )),
            subject,
            // Thread the corrected resend onto the failed mail's thread.
            thread_message_id: str_at(Some(m), "message_id").or(Some(id)),
        });
    }
}

/// Budget keys for one failure: conversation id + normalized recipient, with
/// the raw chat target as a last-resort fallback so an unresolvable failure
/// still gets a cap. Phone numbers are digit-normalized so `+1 (603) 555-0100`
/// and `+16035550100` collapse to one counter.
fn budget_keys(
    mode: &str,
    conversation_id: Option<&str>,
    target: Option<&str>,
    chat_id: Option<&str>,
) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(conv) = conversation_id.map(str::trim).filter(|c| !c.is_empty()) {
        keys.push(format!("{mode}:conv:{}", conv.to_ascii_lowercase()));
    }
    if let Some(raw) = target.map(str::trim).filter(|t| !t.is_empty()) {
        if mode == "email" {
            keys.push(format!("{mode}:to:{}", raw.to_ascii_lowercase()));
        } else {
            let d = digits(raw);
            if !d.is_empty() {
                keys.push(format!("{mode}:to:{d}"));
            }
        }
    }
    if keys.is_empty()
        && let Some(chat) = chat_id.map(str::trim).filter(|c| !c.is_empty())
    {
        keys.push(format!("{mode}:chat:{chat}"));
    }
    keys
}

/// Classify a send-time SDK error: what to tell the agent, and whether the
/// failure is transient. Transient = carrier hiccups, 5xx, timeouts — the
/// transport retries those itself, so waking the agent would double-send.
/// Everything else (content policy, opt-out, bad address, too-long,
/// rate-limit, permanent rejects) wakes the agent.
fn classify_send_error(err: &InkboxError) -> SendFailure {
    match err {
        InkboxError::Api {
            status_code,
            detail,
        } => {
            let (mut code, message, rule) = match detail {
                ApiErrorDetail::Structured(v) => (
                    v.get("error")
                        .or_else(|| v.get("error_code"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    v.get("message")
                        .or_else(|| v.get("detail"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    v.get("rule").and_then(Value::as_str).map(str::to_string),
                ),
                ApiErrorDetail::Message(s) => (None, Some(s.clone()), None),
            };
            // Surface the policy rule inline so the agent knows exactly what
            // to fix, e.g. `message_blocked_spam_filter rule=emoji_overload`.
            if let (Some(c), Some(rule)) = (code.as_deref(), rule.as_deref()) {
                code = Some(format!("{c} rule={rule}"));
            }
            let message = message.unwrap_or_else(|| err.to_string());
            let retryable = matches!(code.as_deref(), Some("carrier_unavailable"))
                || matches!(status_code, 408 | 500 | 502 | 503 | 504)
                || {
                    let lower = message.to_ascii_lowercase();
                    !(400..500).contains(status_code)
                        && (lower.contains("timeout")
                            || lower.contains("temporar")
                            || lower.contains("connection"))
                };
            SendFailure {
                error_code: code,
                error_detail: message,
                retryable,
            }
        }
        InkboxError::RecipientBlocked { reason, .. } => SendFailure {
            error_code: Some("recipient_blocked".into()),
            error_detail: reason.clone(),
            retryable: false,
        },
        // Local validation caught it before any request (e.g. body too long).
        InkboxError::InvalidArgument(m) => SendFailure {
            error_code: Some("invalid_argument".into()),
            error_detail: m.clone(),
            retryable: false,
        },
        // Connectivity: the transport's own retries handle these.
        InkboxError::Transport(_) | InkboxError::Tunnel(_) => SendFailure {
            error_code: None,
            error_detail: err.to_string(),
            retryable: true,
        },
        _ => SendFailure {
            error_code: Some("sdk_error".into()),
            error_detail: err.to_string(),
            retryable: false,
        },
    }
}

/// Per-channel guidance appended to the wake-up: what a deliverable message
/// looks like on this surface, and when to stop instead.
fn channel_guidance(mode: &str) -> &'static str {
    match mode {
        "imessage" => {
            "Rewrite the message so it no longer trips the stated rule and it reads like a \
             human text: plain conversational prose, no markdown. If the recipient has opted \
             out of messages, respect that and stop. Then send the corrected reply now if one \
             is still appropriate."
        }
        "email" => {
            "The receiving mail server did not accept this message — the address may be wrong \
             or the mailbox unreachable. A plain reply here retries the SAME address, so first \
             check the contact card for a corrected address or reach the person on another \
             channel with your tools; only resend here if you have reason to think it will now \
             deliver."
        }
        _ => {
            "Rewrite the message so it no longer trips the stated rule and it reads like a \
             human text: plain conversational prose, no markdown (**bold**, # headers, ``` \
             fences), at most one emoji, no profanity, no test/probe phrasing. Then send the \
             corrected reply now."
        }
    }
}

/// The synthetic wake-up injected into the conversation: a machine-parseable
/// marker line, the exact failure, the undelivered body, per-channel fix
/// guidance, and the remaining budget with a `[SILENT]` escape hatch.
fn wake_text(note: &FailureNote, attempts: u32) -> String {
    let target_part = note
        .target
        .as_deref()
        .map(|t| format!(" to={t}"))
        .unwrap_or_default();
    let conversation_part = note
        .conversation_id
        .as_deref()
        .map(|c| format!(" conversation_id={c}"))
        .unwrap_or_default();
    let failure_line = match (
        note.error_code.as_deref().filter(|c| !c.is_empty()),
        note.error_detail
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty()),
    ) {
        (Some(code), Some(detail)) => format!("[{code}] {detail}"),
        (Some(code), None) => format!("[{code}] the message was not delivered"),
        (None, Some(detail)) => detail.to_string(),
        (None, None) => "the message was not delivered".to_string(),
    };
    let snippet = truncate(note.failed_body.trim(), BODY_SNIPPET_CHARS);
    let remaining = MAX_ATTEMPTS - attempts;
    format!(
        "[inkbox:delivery_failure channel={mode} stage={stage} attempt={attempts}/{MAX_ATTEMPTS}\
         {target_part}{conversation_part}]\n\
         Your outbound {mode} message was NOT delivered — the recipient never saw it.\n\
         Failure: {failure_line}\n\
         Undelivered message:\n\
         «{snippet}»\n\
         {guidance}\n\
         This reply has now failed {attempts} of {MAX_ATTEMPTS} allowed sends; {remaining} left \
         before the thread goes quiet. Send the corrected message as a normal reply in this \
         conversation. Do not mention this delivery problem to the recipient. If there is \
         nothing sensible to send, reply exactly [SILENT].",
        mode = note.mode,
        stage = note.stage,
        guidance = channel_guidance(note.mode),
    )
}

/// SMS reply target: prefer the conversation row; a bare number must route via
/// `smsto:` (send by `to`, not `conversation_id`).
fn sms_reply_target(conversation_id: Option<&str>, remote: Option<&str>) -> Option<String> {
    if let Some(conv) = conversation_id.filter(|c| !c.is_empty()) {
        return Some(format!("sms:{conv}"));
    }
    remote
        .filter(|r| !r.is_empty())
        .map(|r| format!("smsto:{r}"))
}

/// Non-empty trimmed string field on an optional JSON object.
fn str_at(v: Option<&Value>, key: &str) -> Option<String> {
    v?.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Keep only digits, so number formatting differences collapse to one key.
fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Truncate to `max` chars on a char boundary, with a `…` marker.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Mask a phone number / address for logs: keep the mode tag and last 4 chars.
fn mask_target(t: &str) -> String {
    let tail: String = t
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tracker() -> (FailureTracker, mpsc::Receiver<ChannelMessage>) {
        let (tx, rx) = mpsc::channel(8);
        let t = FailureTracker::new("zc");
        t.set_sender(tx);
        (t, rx)
    }

    #[test]
    fn budget_keys_normalize_numbers_and_emails() {
        let keys = budget_keys("sms", Some("Conv-7"), Some("+1 (603) 555-0100"), None);
        assert_eq!(keys, vec!["sms:conv:conv-7", "sms:to:16035550100"]);
        assert_eq!(
            budget_keys("email", None, Some("Bob@Example.com"), None),
            vec!["email:to:bob@example.com"]
        );
        // The chat fallback applies only when nothing else resolved.
        assert_eq!(
            budget_keys("sms", None, None, Some("smsto:+15550100")),
            vec!["sms:chat:smsto:+15550100"]
        );
        assert_eq!(
            budget_keys("sms", Some("c1"), None, Some("chat")),
            vec!["sms:conv:c1"]
        );
        assert!(budget_keys("sms", None, None, None).is_empty());
    }

    #[test]
    fn budget_merges_by_max_across_keys_and_caps_at_three() {
        let (t, mut rx) = tracker();
        let note = |conv: Option<&str>, target: Option<&str>| FailureNote {
            mode: "sms",
            stage: "delivery_failed",
            conversation_id: conv.map(str::to_string),
            target: target.map(str::to_string),
            chat_id: None,
            reply_target: "sms:c1".into(),
            failed_body: "hello".into(),
            error_code: Some("carrier_rejected".into()),
            error_detail: None,
            subject: None,
            thread_message_id: None,
        };
        // Failure 1 keyed by conversation only; failure 2 keyed by number only
        // but sharing the conversation — the counters merge via write-back.
        t.note_failure(note(Some("c1"), None));
        assert!(rx.try_recv().is_ok());
        t.note_failure(note(Some("c1"), Some("+15550100")));
        let wake2 = rx.try_recv().expect("second failure wakes");
        assert!(wake2.content.contains("attempt=2/3"));
        // Failure 3 by number alone still sees the merged count — quiet.
        t.note_failure(note(None, Some("+1 555 0100")));
        assert!(rx.try_recv().is_err(), "cap reached: no wake");
    }

    #[test]
    fn clear_resets_the_budget() {
        let (t, mut rx) = tracker();
        let note = FailureNote {
            mode: "sms",
            stage: "send_rejected",
            conversation_id: Some("c1".into()),
            target: None,
            chat_id: None,
            reply_target: "sms:c1".into(),
            failed_body: "x".into(),
            error_code: None,
            error_detail: None,
            subject: None,
            thread_message_id: None,
        };
        for _ in 0..2 {
            t.note_failure(FailureNote { ..copy(&note) });
        }
        t.clear("sms", Some("c1"), None, None);
        t.note_failure(copy(&note));
        // After the reset this counts as attempt 1 again.
        let mut last = None;
        while let Ok(m) = rx.try_recv() {
            last = Some(m);
        }
        assert!(last.unwrap().content.contains("attempt=1/3"));
    }

    /// FailureNote has no Clone (single-use by design); tests copy by hand.
    fn copy(n: &FailureNote) -> FailureNote {
        FailureNote {
            mode: n.mode,
            stage: n.stage,
            conversation_id: n.conversation_id.clone(),
            target: n.target.clone(),
            chat_id: n.chat_id.clone(),
            reply_target: n.reply_target.clone(),
            failed_body: n.failed_body.clone(),
            error_code: n.error_code.clone(),
            error_detail: n.error_detail.clone(),
            subject: n.subject.clone(),
            thread_message_id: n.thread_message_id.clone(),
        }
    }

    #[test]
    fn text_delivery_failed_wakes_with_recipient_row_fallback() {
        let (t, mut rx) = tracker();
        let payload = json!({
            "event_type": "text.delivery_failed",
            "data": {
                "recipient_phone_number": "+15550100",
                "text_message": {
                    "id": "t1",
                    "direction": "outbound",
                    "conversation_id": "c9",
                    "remote_phone_number": "+15550100",
                    "text": "the failed body",
                    "recipients": [
                        { "recipient_phone_number": "+1 (555) 0100",
                          "error_code": "40002", "error_detail": "carrier flagged" }
                    ]
                }
            }
        });
        t.observe_event(&payload);
        let wake = rx.try_recv().expect("delivery failure wakes");
        assert_eq!(wake.reply_target, "sms:c9");
        assert!(wake.content.contains("[40002] carrier flagged"));
        assert!(wake.content.contains("«the failed body»"));
        assert!(wake.content.contains("[SILENT]"));
        // A webhook replay of the same message id is deduped.
        t.observe_event(&payload);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn inbound_direction_and_missing_id_are_ignored() {
        let (t, mut rx) = tracker();
        t.observe_event(&json!({
            "event_type": "text.delivery_failed",
            "data": { "text_message": { "id": "t2", "direction": "inbound", "text": "x" } }
        }));
        t.observe_event(&json!({
            "event_type": "imessage.delivery_failed",
            "data": { "message": { "direction": "outbound", "content": "x" } }
        }));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn imessage_failure_targets_the_conversation() {
        let (t, mut rx) = tracker();
        t.observe_event(&json!({
            "event_type": "imessage.delivery_failed",
            "data": { "message": {
                "id": "m1", "direction": "outbound", "conversation_id": "ic-2",
                "remote_number": "+15550100", "content": "hey",
                "error_code": "opted_out", "error_message": "recipient opted out"
            } }
        }));
        let wake = rx.try_recv().expect("imessage failure wakes");
        assert_eq!(wake.reply_target, "imessage:ic-2");
        assert!(wake.content.contains("[opted_out] recipient opted out"));
    }

    #[test]
    fn mail_bounce_synthesizes_detail_and_threads_by_message_id() {
        let (t, mut rx) = tracker();
        t.observe_event(&json!({
            "event_type": "message.bounced",
            "data": { "message": {
                "id": "row-1", "message_id": "<m1@mail>", "direction": "outbound",
                "to_addresses": ["bob@example.com"], "subject": "Hi",
                "status": "bounced", "snippet": "original body"
            } }
        }));
        let wake = rx.try_recv().expect("bounce wakes");
        assert_eq!(wake.reply_target, "email:bob@example.com");
        assert_eq!(wake.id, "<m1@mail>"); // corrected resend threads
        assert_eq!(wake.subject.as_deref(), Some("Hi"));
        assert!(wake.content.contains("was returned as bounced"));
        assert!(wake.content.contains("stage=bounced"));
    }

    #[test]
    fn delivered_receipt_resets_the_budget() {
        let (t, mut rx) = tracker();
        let fail = json!({
            "event_type": "text.delivery_failed",
            "data": { "text_message": {
                "id": "t3", "direction": "outbound", "conversation_id": "c1",
                "remote_phone_number": "+15550100", "text": "x", "error_code": "40002"
            } }
        });
        t.observe_event(&fail);
        t.observe_event(&fail); // deduped replay, budget unchanged
        t.observe_event(&json!({
            "event_type": "text.delivered",
            "data": { "text_message": { "conversation_id": "c1", "remote_phone_number": "+15550100" } }
        }));
        let mut fail2 = fail.clone();
        fail2["data"]["text_message"]["id"] = json!("t4");
        t.observe_event(&fail2);
        let mut last = None;
        while let Ok(m) = rx.try_recv() {
            last = Some(m);
        }
        assert!(last.unwrap().content.contains("attempt=1/3"));
    }

    #[test]
    fn classify_send_error_splits_transient_from_fixable() {
        let api = |status: u16, detail: Value| InkboxError::Api {
            status_code: status,
            detail: ApiErrorDetail::Structured(detail),
        };
        // Content-policy block surfaces the rule inline.
        let f = classify_send_error(&api(
            422,
            json!({ "error": "message_blocked_spam_filter", "rule": "emoji_overload",
                    "message": "too many emoji" }),
        ));
        assert!(!f.retryable);
        assert_eq!(
            f.error_code.as_deref(),
            Some("message_blocked_spam_filter rule=emoji_overload")
        );
        assert_eq!(f.error_detail, "too many emoji");
        // Opt-out wakes; carrier hiccup does not.
        assert!(
            !classify_send_error(&api(402, json!({ "error": "recipient_opted_out" }))).retryable
        );
        assert!(classify_send_error(&api(503, json!({ "error": "upstream" }))).retryable);
        assert!(
            classify_send_error(&api(400, json!({ "error": "carrier_unavailable" }))).retryable
        );
        // 4xx with a timeout-sounding message still wakes (status wins).
        assert!(!classify_send_error(&api(422, json!({ "message": "timeout parsing" }))).retryable);
    }

    #[test]
    fn wake_reuses_the_inbound_sender_label() {
        let (t, mut rx) = tracker();
        // The inbound mapping resolved this counterparty to a contact name.
        t.remember_sender("sms:c9", "Alice");
        t.observe_event(&json!({
            "event_type": "text.delivery_failed",
            "data": { "text_message": {
                "id": "t9", "direction": "outbound", "conversation_id": "c9",
                "remote_phone_number": "+15550100", "text": "x", "error_code": "40002"
            } }
        }));
        // Same sender + reply target = same sender-scoped session.
        assert_eq!(rx.try_recv().unwrap().sender, "Alice");
    }

    #[test]
    fn wake_text_truncates_the_body_snippet() {
        let note = FailureNote {
            mode: "sms",
            stage: "send_rejected",
            conversation_id: None,
            target: Some("+15550100".into()),
            chat_id: None,
            reply_target: "smsto:+15550100".into(),
            failed_body: "x".repeat(500),
            error_code: None,
            error_detail: None,
            subject: None,
            thread_message_id: None,
        };
        let text = wake_text(&note, 1);
        assert!(text.contains(&format!("{}…", "x".repeat(BODY_SNIPPET_CHARS))));
        assert!(text.contains("attempt=1/3 to=+15550100"));
        assert!(text.contains("2 left"));
    }
}
