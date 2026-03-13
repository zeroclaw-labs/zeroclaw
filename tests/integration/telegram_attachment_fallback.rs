//! Regression tests for Telegram attachment fallback behavior.
//!
//! When sending media by URL fails (e.g. Telegram can't fetch the URL or the
//! content type is wrong), the channel should fall back to sending the URL as
//! a text link instead of losing the entire reply.
//!
//! Bug: Previously, `send_attachment()` would propagate the error from
//! `send_document_by_url()` immediately via `?`, causing the entire reply
//! (including already-sent text) to fail with no fallback.

use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroclaw::channels::telegram::TelegramChannel;
use zeroclaw::channels::traits::{Channel, SendMessage};

/// Helper: create a TelegramChannel pointing at a mock server.
fn test_channel(mock_url: &str) -> TelegramChannel {
    TelegramChannel::new("TEST_TOKEN".into(), vec!["*".into()], false)
        .with_api_base(mock_url.to_string())
}

/// Helper: mount a mock that accepts sendMessage requests (the fallback path).
async fn mock_send_message_ok(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path_regex(r"/botTEST_TOKEN/sendMessage$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": {
                "message_id": 1,
                "chat": {"id": 123},
                "text": "ok"
            }
        })))
        .expect(1..)
        .mount(server)
        .await;
}

/// When sendDocument by URL fails with "wrong type of the web page content",
/// the channel should fall back to sending the URL as a text link.
#[tokio::test]
async fn document_url_failure_falls_back_to_text_link() {
    let server = MockServer::start().await;

    // sendDocument returns 400 (simulates Telegram rejecting the URL)
    Mock::given(method("POST"))
        .and(path_regex(r"/botTEST_TOKEN/sendDocument$"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "ok": false,
            "error_code": 400,
            "description": "Bad Request: wrong type of the web page content"
        })))
        .expect(1)
        .mount(&server)
        .await;

    // sendMessage should succeed (this is the fallback)
    mock_send_message_ok(&server).await;

    let channel = test_channel(&server.uri());
    let msg = SendMessage::new(
        "Here is the report [DOCUMENT:https://example.com/page.html]",
        "123",
    );

    // This should NOT error — it should fall back to text
    let result = channel.send(&msg).await;
    assert!(
        result.is_ok(),
        "send should succeed via text fallback, got: {result:?}"
    );
}

/// When sendPhoto by URL fails, the channel should fall back to text link.
#[tokio::test]
async fn photo_url_failure_falls_back_to_text_link() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(r"/botTEST_TOKEN/sendPhoto$"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "ok": false,
            "error_code": 400,
            "description": "Bad Request: failed to get HTTP URL content"
        })))
        .expect(1)
        .mount(&server)
        .await;

    mock_send_message_ok(&server).await;

    let channel = test_channel(&server.uri());
    let msg = SendMessage::new(
        "Check this [IMAGE:https://internal-server.local/screenshot.png]",
        "456",
    );

    let result = channel.send(&msg).await;
    assert!(
        result.is_ok(),
        "send should succeed via text fallback, got: {result:?}"
    );
}

/// Text portion of a message with attachments is still delivered even when
/// the attachment fails.
#[tokio::test]
async fn text_portion_delivered_before_attachment_failure() {
    let server = MockServer::start().await;

    // sendDocument fails
    Mock::given(method("POST"))
        .and(path_regex(r"/botTEST_TOKEN/sendDocument$"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "ok": false,
            "error_code": 400,
            "description": "Bad Request: wrong type of the web page content"
        })))
        .expect(1)
        .mount(&server)
        .await;

    // sendMessage should be called at least twice:
    // 1. for the text portion ("Here is the file")
    // 2. for the fallback text link
    Mock::given(method("POST"))
        .and(path_regex(r"/botTEST_TOKEN/sendMessage$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": {
                "message_id": 1,
                "chat": {"id": 789},
                "text": "ok"
            }
        })))
        .expect(2)
        .mount(&server)
        .await;

    let channel = test_channel(&server.uri());
    let msg = SendMessage::new(
        "Here is the file [DOCUMENT:https://example.com/report.html]",
        "789",
    );

    let result = channel.send(&msg).await;
    assert!(result.is_ok(), "send should succeed, got: {result:?}");
}

/// When multiple attachments are present and one fails, the others should
/// still be attempted (each gets its own fallback).
#[tokio::test]
async fn multiple_attachments_independent_fallback() {
    let server = MockServer::start().await;

    // sendDocument fails (for the .html attachment)
    Mock::given(method("POST"))
        .and(path_regex(r"/botTEST_TOKEN/sendDocument$"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "ok": false,
            "error_code": 400,
            "description": "Bad Request: wrong type of the web page content"
        })))
        .expect(1)
        .mount(&server)
        .await;

    // sendPhoto also fails
    Mock::given(method("POST"))
        .and(path_regex(r"/botTEST_TOKEN/sendPhoto$"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "ok": false,
            "error_code": 400,
            "description": "Bad Request: failed to get HTTP URL content"
        })))
        .expect(1)
        .mount(&server)
        .await;

    // sendMessage succeeds (text + 2 fallback links)
    Mock::given(method("POST"))
        .and(path_regex(r"/botTEST_TOKEN/sendMessage$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": {
                "message_id": 1,
                "chat": {"id": 100},
                "text": "ok"
            }
        })))
        .expect(3) // text + doc fallback + image fallback
        .mount(&server)
        .await;

    let channel = test_channel(&server.uri());
    let msg = SendMessage::new(
        "Files: [DOCUMENT:https://example.com/page.html] and [IMAGE:https://internal.local/pic.png]",
        "100",
    );

    let result = channel.send(&msg).await;
    assert!(
        result.is_ok(),
        "send should succeed with fallbacks for all attachments, got: {result:?}"
    );
}

/// When attachment succeeds, no fallback text is sent.
#[tokio::test]
async fn successful_attachment_no_fallback() {
    let server = MockServer::start().await;

    // sendDocument succeeds
    Mock::given(method("POST"))
        .and(path_regex(r"/botTEST_TOKEN/sendDocument$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": {
                "message_id": 2,
                "chat": {"id": 200},
                "document": {"file_id": "abc"}
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    // sendMessage should only be called once (for the text portion),
    // NOT a second time for a fallback
    Mock::given(method("POST"))
        .and(path_regex(r"/botTEST_TOKEN/sendMessage$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": {
                "message_id": 1,
                "chat": {"id": 200},
                "text": "ok"
            }
        })))
        .expect(1) // only the text portion, no fallback
        .mount(&server)
        .await;

    let channel = test_channel(&server.uri());
    let msg = SendMessage::new(
        "Report attached [DOCUMENT:https://example.com/report.pdf]",
        "200",
    );

    let result = channel.send(&msg).await;
    assert!(
        result.is_ok(),
        "send should succeed normally, got: {result:?}"
    );
}

/// Document-only message (no text) with URL failure should still send
/// a fallback text link.
#[tokio::test]
async fn document_only_message_falls_back_to_text() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex(r"/botTEST_TOKEN/sendDocument$"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "ok": false,
            "error_code": 400,
            "description": "Bad Request: failed to get HTTP URL content"
        })))
        .expect(1)
        .mount(&server)
        .await;

    // Fallback text link
    Mock::given(method("POST"))
        .and(path_regex(r"/botTEST_TOKEN/sendMessage$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": {
                "message_id": 1,
                "chat": {"id": 300},
                "text": "ok"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let channel = test_channel(&server.uri());
    // Message is ONLY the attachment marker — no surrounding text
    let msg = SendMessage::new("[DOCUMENT:https://example.com/file.html]", "300");

    let result = channel.send(&msg).await;
    assert!(
        result.is_ok(),
        "document-only message should fall back to text, got: {result:?}"
    );
}
