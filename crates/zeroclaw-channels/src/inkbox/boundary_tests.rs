//! Boundary tests: the real signed webhook handler over HTTP, the real
//! channel `send()` through the real (blocking) SDK, against a mocked Inkbox
//! API. Only the agent's model turn is scripted — every boundary the channel
//! owns (signature verification, HTTP routing, SDK wire calls) is crossed for
//! real, so these prove the delivery-failure loop and the per-surface
//! round-trips end to end without a live account.

use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use tokio::sync::mpsc;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

use super::InkboxChannel;
use super::inbound;

const KEY: &str = "test-signing-key";
const HANDLE: &str = "support-bot";
const PHONE_ID: &str = "44444444-4444-4444-4444-444444444444";
const MAILBOX: &str = "support-bot@inkboxmail.com";

/// The identity payload `send()` resolves before every delivery.
fn identity_json() -> Value {
    json!({
        "id": "11111111-1111-1111-1111-111111111111",
        "organization_id": "org_x",
        "agent_handle": HANDLE,
        "created_at": "2026-06-01T00:00:00+00:00",
        "updated_at": "2026-06-01T00:00:00+00:00",
        "imessage_enabled": true,
        "mailbox": {
            "id": "22222222-2222-2222-2222-222222222222",
            "email_address": MAILBOX,
            "created_at": "2026-06-01T00:00:00+00:00",
            "updated_at": "2026-06-01T00:00:00+00:00"
        },
        "phone_number": {
            "id": PHONE_ID,
            "number": "+15550001111",
            "type": "local",
            "status": "active",
            "incoming_call_action": "webhook",
            "created_at": "2026-06-01T00:00:00+00:00",
            "updated_at": "2026-06-01T00:00:00+00:00"
        }
    })
}

fn sent_text_json() -> Value {
    json!({
        "id": "66666666-6666-6666-6666-666666666666",
        "direction": "outbound",
        "local_phone_number": "+15550001111",
        "type": "sms",
        "is_read": false,
        "created_at": "2026-06-01T00:00:00+00:00",
        "updated_at": "2026-06-01T00:00:00+00:00"
    })
}

/// One running stack: mocked Inkbox API, a real channel bound to it, and the
/// channel's loopback webhook server, plus the orchestrator-side receiver.
struct Stack {
    api: MockServer,
    channel: InkboxChannel,
    webhook_url: String,
    rx: mpsc::Receiver<ChannelMessage>,
    http: reqwest::Client,
}

async fn start_stack() -> Stack {
    let api = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/identities/{HANDLE}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(identity_json()))
        .mount(&api)
        .await;

    // The blocking SDK client spins its own runtime; build (and later drop)
    // it off the test runtime.
    let base = api.uri();
    let client = std::thread::spawn(move || {
        inkbox::Inkbox::builder("ApiKey_test")
            .base_url(&base)
            .build()
            .expect("client builds")
    })
    .join()
    .expect("client thread");
    let channel = InkboxChannel::new(client, HANDLE, KEY, "zc");

    // The channel's loopback webhook server, exactly as `listen` builds it.
    let (tx, rx) = mpsc::channel(16);
    channel.failure.set_sender(tx.clone());
    let app = inbound::router(inbound::AppState {
        tx,
        failure: channel.failure.clone(),
        signing_key: KEY.to_string(),
        alias: "zc".to_string(),
        public_host: "example.test".to_string(),
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let webhook_url = format!("http://{}/webhook", listener.local_addr().unwrap());
    zeroclaw_spawn::spawn!(axum::serve(listener, app).into_future());

    Stack {
        api,
        channel,
        webhook_url,
        rx,
        http: reqwest::Client::new(),
    }
}

impl Stack {
    /// POST an event to the channel's webhook endpoint, signed the way the
    /// Inkbox server signs webhooks: HMAC over `"{request_id}.{timestamp}."`
    /// plus the raw body.
    async fn post_signed(&self, event: &Value) -> u16 {
        let body = event.to_string();
        let request_id = uuid::Uuid::new_v4().to_string();
        let timestamp = super::now_secs().to_string();
        let mut mac = Hmac::<Sha256>::new_from_slice(KEY.as_bytes()).unwrap();
        mac.update(format!("{request_id}.{timestamp}.").as_bytes());
        mac.update(body.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());
        self.http
            .post(&self.webhook_url)
            .header("x-inkbox-request-id", request_id)
            .header("x-inkbox-timestamp", timestamp)
            .header("x-inkbox-signature", format!("sha256={sig}"))
            .body(body)
            .send()
            .await
            .expect("webhook POST")
            .status()
            .as_u16()
    }

    async fn recv(&mut self) -> ChannelMessage {
        tokio::time::timeout(std::time::Duration::from_secs(5), self.rx.recv())
            .await
            .expect("message within 5s")
            .expect("channel open")
    }

    /// Bodies of every request the mocked API saw on `path_part`, in order.
    async fn api_bodies(&self, path_part: &str) -> Vec<Value> {
        self.api
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().contains(path_part))
            .map(|r| serde_json::from_slice(&r.body).unwrap_or(Value::Null))
            .collect()
    }
}

/// The delivery-failure loop end to end: a fresh inbound arrives through the
/// signed handler; the agent's reply is rejected by the outbound content
/// policy at the API boundary; the wake reaches the same sender-scoped
/// session with the exact error and the undelivered body; the corrected
/// resend goes out exactly once; a delivered receipt resets the budget; and
/// the async webhook failure surface draws on the same loop.
#[tokio::test(flavor = "multi_thread")]
async fn delivery_failure_loop_crosses_every_channel_boundary() {
    let mut stack = start_stack().await;

    // The first send is rejected by the outbound content policy; later sends
    // are accepted. (Mount order: the bounded mock consumes first.)
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/phone/numbers/{PHONE_ID}/texts")))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "detail": {
                "error": "message_blocked_spam_filter",
                "rule": "emoji_overload",
                "message": "too many emoji for carrier delivery"
            }
        })))
        .up_to_n_times(1)
        .mount(&stack.api)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/phone/numbers/{PHONE_ID}/texts")))
        .respond_with(ResponseTemplate::new(200).set_body_json(sent_text_json()))
        .mount(&stack.api)
        .await;

    // Fresh inbound SMS through the signed webhook handler.
    let status = stack
        .post_signed(&json!({
            "event_type": "text.received",
            "data": {
                "text_message": {
                    "id": "t1",
                    "direction": "inbound",
                    "conversation_id": "c1",
                    "remote_phone_number": "+15550002222",
                    "text": "can you send me the details?"
                },
                "contacts": [{ "id": "ct1", "name": "Alice" }]
            }
        }))
        .await;
    assert_eq!(status, 200);
    let inbound_msg = stack.recv().await;
    assert_eq!(inbound_msg.reply_target, "sms:c1");
    assert_eq!(inbound_msg.sender, "Alice");

    // Scripted agent turn 1: an undeliverable reply. The channel's real send
    // path carries it to the (mocked) API, which rejects it.
    let bad_reply = "🎉🎉🎉 **DETAILS** 🎉🎉🎉";
    let err = stack
        .channel
        .send(&SendMessage::reply_to(&inbound_msg, bad_reply))
        .await;
    assert!(err.is_err(), "content-policy rejection surfaces");

    // The wake lands in the same sender-scoped session with the whole story.
    let wake = stack.recv().await;
    assert_eq!(wake.reply_target, "sms:c1");
    assert_eq!(wake.sender, "Alice", "wake joins the inbound session");
    assert!(
        wake.content
            .contains("[inkbox:delivery_failure channel=sms stage=send_rejected attempt=1/3"),
        "marker line: {}",
        wake.content
    );
    assert!(
        wake.content
            .contains("[message_blocked_spam_filter rule=emoji_overload]"),
        "exact error surfaces: {}",
        wake.content
    );
    assert!(
        wake.content.contains(bad_reply),
        "undelivered body is echoed back"
    );
    assert!(wake.content.contains("[SILENT]"), "escape hatch offered");

    // Scripted agent turn 2: the corrected resend, which the API accepts.
    let fixed = "Sounds good. The details are on their way by email.";
    stack
        .channel
        .send(&SendMessage::reply_to(&wake, fixed))
        .await
        .expect("corrected resend delivers");

    // Exactly two sends crossed the wire: the rejected one and the fix.
    let sends = stack.api_bodies("/texts").await;
    assert_eq!(sends.len(), 2, "no duplicate sends");
    assert_eq!(sends[0]["text"], bad_reply);
    assert_eq!(sends[1]["text"], fixed);
    assert_eq!(sends[1]["conversation_id"], "c1");

    // Delivered receipt through the signed handler resets the budget...
    stack
        .post_signed(&json!({
            "event_type": "text.delivered",
            "data": { "text_message": {
                "id": "t2", "direction": "outbound",
                "conversation_id": "c1", "remote_phone_number": "+15550002222"
            } }
        }))
        .await;
    // ...so a later carrier failure (the async webhook surface) starts a
    // fresh budget and wakes again.
    stack
        .post_signed(&json!({
            "event_type": "text.delivery_failed",
            "data": { "text_message": {
                "id": "t3", "direction": "outbound",
                "conversation_id": "c1", "remote_phone_number": "+15550002222",
                "text": "follow-up that the carrier flagged",
                "error_code": "40002", "error_detail": "carrier flagged"
            } }
        }))
        .await;
    let wake2 = stack.recv().await;
    assert!(
        wake2.content.contains("stage=delivery_failed attempt=1/3"),
        "delivered receipt reset the budget: {}",
        wake2.content
    );
    assert!(wake2.content.contains("[40002] carrier flagged"));
}

/// The hard cap through the real webhook boundary: failures 1 and 2 wake the
/// agent, failure 3 goes quiet, and replayed webhooks never double-count.
#[tokio::test(flavor = "multi_thread")]
async fn retry_budget_caps_at_three_and_dedupes_replays() {
    let mut stack = start_stack().await;

    let failure = |id: &str| {
        json!({
            "event_type": "text.delivery_failed",
            "data": { "text_message": {
                "id": id, "direction": "outbound",
                "conversation_id": "c9", "remote_phone_number": "+15550003333",
                "text": "x", "error_code": "40002"
            } }
        })
    };
    stack.post_signed(&failure("f1")).await;
    stack.post_signed(&failure("f1")).await; // replay: deduped
    stack.post_signed(&failure("f2")).await;
    stack.post_signed(&failure("f3")).await; // third strike: quiet

    let w1 = stack.recv().await;
    let w2 = stack.recv().await;
    assert!(w1.content.contains("attempt=1/3"));
    assert!(w2.content.contains("attempt=2/3"));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), stack.rx.recv())
            .await
            .is_err(),
        "the capped failure must not wake the agent"
    );
}

/// Email and iMessage round trips across the same boundaries: signed inbound
/// webhook in, threaded reply out through the real SDK to the mocked API.
/// An unsigned webhook is rejected outright.
#[tokio::test(flavor = "multi_thread")]
async fn email_and_imessage_round_trip_and_unsigned_webhooks_are_dropped() {
    let mut stack = start_stack().await;

    Mock::given(method("POST"))
        .and(path_regex(r"^/api/v1/mail/mailboxes/.+/messages$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "77777777-7777-7777-7777-777777777777",
            "mailbox_id": "22222222-2222-2222-2222-222222222222",
            "message_id": "<out-1@inkboxmail.com>",
            "from_address": MAILBOX,
            "to_addresses": ["bob@example.com"],
            "direction": "outbound",
            "status": "queued",
            "is_read": false,
            "is_starred": false,
            "has_attachments": false,
            "created_at": "2026-06-01T00:00:00+00:00"
        })))
        .mount(&stack.api)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/imessage/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "message": {
                "id": "88888888-8888-8888-8888-888888888888",
                "conversation_id": "99999999-9999-9999-9999-999999999999",
                "assignment_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "direction": "outbound",
                "remote_number": "+15550002222",
                "message_type": "message",
                "service": "imessage",
                "is_read": false,
                "created_at": "2026-06-01T00:00:00+00:00",
                "updated_at": "2026-06-01T00:00:00+00:00"
            }
        })))
        .mount(&stack.api)
        .await;

    // Email in → threaded reply out.
    stack
        .post_signed(&json!({
            "event_type": "message.received",
            "data": { "message": {
                "id": "row-1", "message_id": "<m1@mail>",
                "from_address": "bob@example.com",
                "subject": "Hello", "snippet": "hi there"
            } }
        }))
        .await;
    let mail_in = stack.recv().await;
    assert_eq!(mail_in.reply_target, "email:bob@example.com");
    stack
        .channel
        .send(&SendMessage::reply_to(&mail_in, "Hi Bob"))
        .await
        .expect("email reply sends");
    let mails = stack.api_bodies("/messages").await;
    let mail = mails
        .iter()
        .find(|b| b.get("subject").is_some())
        .expect("mail send crossed the wire");
    assert_eq!(mail["subject"], "Re: Hello");
    assert_eq!(mail["in_reply_to_message_id"], "<m1@mail>");
    assert_eq!(mail["recipients"]["to"][0], "bob@example.com");
    assert_eq!(mail["body_text"], "Hi Bob");

    // iMessage in → conversation-bound reply out.
    let convo = "99999999-9999-9999-9999-999999999999";
    stack
        .post_signed(&json!({
            "event_type": "imessage.received",
            "data": { "message": {
                "id": "im-1", "conversation_id": convo,
                "remote_number": "+15550002222", "content": "you there?"
            } }
        }))
        .await;
    let im_in = stack.recv().await;
    assert_eq!(im_in.reply_target, format!("imessage:{convo}"));
    stack
        .channel
        .send(&SendMessage::reply_to(&im_in, "Here now."))
        .await
        .expect("imessage reply sends");
    let ims = stack.api_bodies("/imessage/messages").await;
    let im = ims.last().expect("imessage send crossed the wire");
    assert_eq!(im["conversation_id"], convo);
    assert_eq!(im["text"], "Here now.");

    // An unsigned webhook never reaches the pipeline.
    let unsigned = stack
        .http
        .post(&stack.webhook_url)
        .body(json!({ "event_type": "text.received" }).to_string())
        .send()
        .await
        .unwrap()
        .status()
        .as_u16();
    assert_eq!(unsigned, 401);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), stack.rx.recv())
            .await
            .is_err(),
        "unsigned events enqueue nothing"
    );
}
