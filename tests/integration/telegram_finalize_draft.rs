use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroclaw::channels::Channel;
use zeroclaw::channels::telegram::TelegramChannel;

fn test_channel(mock_url: &str) -> TelegramChannel {
    TelegramChannel::new("TEST_TOKEN".into(), vec!["*".into()], false)
        .with_api_base(mock_url.to_string())
}

fn telegram_ok_response(message_id: i64) -> serde_json::Value {
    json!({
        "ok": true,
        "result": {
            "message_id": message_id,
            "chat": {"id": 123},
            "text": "ok"
        }
    })
}

fn telegram_error_response(description: &str) -> serde_json::Value {
    json!({
        "ok": false,
        "error_code": 400,
        "description": description,
    })
}

#[tokio::test]
async fn finalize_draft_treats_not_modified_as_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/botTEST_TOKEN/editMessageText"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(telegram_error_response(
                "Bad Request: message is not modified",
            )),
        )
        .mount(&server)
        .await;

    let channel = test_channel(&server.uri());
    let result = channel.finalize_draft("123", "42", "final text").await;

    assert!(
        result.is_ok(),
        "not modified should be treated as success, got: {result:?}"
    );

    let requests = server
        .received_requests()
        .await
        .expect("requests should be captured");
    assert_eq!(requests.len(), 1, "should stop after first edit response");
    assert_eq!(requests[0].url.path(), "/botTEST_TOKEN/editMessageText");
}

#[tokio::test]
async fn finalize_draft_plain_retry_treats_not_modified_as_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/botTEST_TOKEN/editMessageText"))
        .and(body_partial_json(json!({
            "chat_id": "123",
            "message_id": 42,
            "parse_mode": "HTML",
        })))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(telegram_error_response("Bad Request: can't parse entities")),
        )
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTEST_TOKEN/editMessageText"))
        .and(body_partial_json(json!({
            "chat_id": "123",
            "message_id": 42,
            "text": "Use **bold**",
        })))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(telegram_error_response(
                "Bad Request: message is not modified",
            )),
        )
        .expect(1)
        .mount(&server)
        .await;

    let channel = test_channel(&server.uri());
    let result = channel.finalize_draft("123", "42", "Use **bold**").await;

    assert!(
        result.is_ok(),
        "plain retry should accept not modified, got: {result:?}"
    );

    let requests = server
        .received_requests()
        .await
        .expect("requests should be captured");
    assert_eq!(requests.len(), 2, "should only attempt the two edit calls");
}

#[tokio::test]
async fn finalize_draft_skips_send_message_when_delete_fails() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/botTEST_TOKEN/editMessageText"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(telegram_error_response(
                "Bad Request: message cannot be edited",
            )),
        )
        .expect(2)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTEST_TOKEN/deleteMessage"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(telegram_error_response(
                "Bad Request: message to delete not found",
            )),
        )
        .expect(1)
        .mount(&server)
        .await;

    let channel = test_channel(&server.uri());
    let result = channel.finalize_draft("123", "42", "final text").await;

    assert!(
        result.is_ok(),
        "delete failure should skip sendMessage instead of erroring, got: {result:?}"
    );

    let requests = server
        .received_requests()
        .await
        .expect("requests should be captured");
    assert_eq!(
        requests
            .iter()
            .filter(|req| req.url.path() == "/botTEST_TOKEN/sendMessage")
            .count(),
        0,
        "sendMessage should be skipped when deleteMessage fails"
    );
}

#[tokio::test]
async fn finalize_draft_sends_fresh_message_after_successful_delete() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/botTEST_TOKEN/editMessageText"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(telegram_error_response(
                "Bad Request: message cannot be edited",
            )),
        )
        .expect(2)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTEST_TOKEN/deleteMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(telegram_ok_response(42)))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/botTEST_TOKEN/sendMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(telegram_ok_response(43)))
        .expect(1)
        .mount(&server)
        .await;

    let channel = test_channel(&server.uri());
    let result = channel.finalize_draft("123", "42", "final text").await;

    assert!(
        result.is_ok(),
        "successful delete should allow safe sendMessage fallback, got: {result:?}"
    );

    let requests = server
        .received_requests()
        .await
        .expect("requests should be captured");
    assert_eq!(
        requests
            .iter()
            .filter(|req| req.url.path() == "/botTEST_TOKEN/sendMessage")
            .count(),
        1,
        "sendMessage should be attempted exactly once after delete succeeds"
    );
}
