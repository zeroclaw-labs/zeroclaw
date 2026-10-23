use super::*;
use serde_json::{Value, json};
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_api::session_events::payloads::{ResponseReadReference, response_references_for_read};
use zeroclaw_kernel::session::query::PayloadSelector;
use zeroclaw_providers::compatible::{AuthStyle, OpenAiCompatibleModelProvider};
use zeroclaw_providers::openai::OpenAiModelProvider;

fn payload_bytes(h: &Harness, reference: &StoredPayloadRef) -> Vec<u8> {
    let PayloadRead::Present(payload) = h
        .store
        .read_payload(&reference.payload_id)
        .expect("captured content")
    else {
        panic!("present content");
    };
    payload.body
}

#[tokio::test]
async fn plain_reasoning_and_unknown_content_are_preserved_both_backends() {
    for backend in ["sqlite", "jsonl"] {
        for (adapter, field) in [
            ("openai", "reasoning_content"),
            ("compatible", "reasoning_content"),
            ("compatible", "reasoning"),
        ] {
            for reasoning in [
                "ordinary token=fixture-token api_key=fixture-key https://example.test/?q=rust#page",
                r#"{"thinking":"ordinary token=fixture-token","signature":"sk-fixture-signature"}"#,
            ] {
                audit(backend, adapter, field, reasoning).await;
            }
        }
    }
}

async fn audit(backend: &str, adapter: &str, field: &str, reasoning: &str) {
    let h = Harness::new(backend);
    let mut message = json!({
        "role":"assistant",
        "content":[
            {"type":"text","text":"answer"},
            {"type":"fixture.future","api_key":"fixture-key","token":"fixture-token"},
            {"type":"gemini.part_replay","thoughtSignature":"sk-fixture-spoof","token":"fixture-token"},
        ],
        "future_extension":{"api_key":{"nested":"fixture-key"},"token":"fixture-token","payload_id":"wire-value"},
    });
    message[field] = Value::String(reasoning.into());
    let wire = json!({"choices":[{"message":message,"finish_reason":"stop"}]});
    let app = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(move || {
            let wire = wire.clone();
            async move { axum::Json(wire) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    let server = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.expect("server");
    });
    let provider: Box<dyn ModelProvider> = if adapter == "openai" {
        Box::new(
            OpenAiModelProvider::builder("fixture")
                .base_url(&url)
                .credential(Some("fixture-key"))
                .build(),
        )
    } else {
        Box::new(
            OpenAiCompatibleModelProvider::builder("fixture")
                .display_name("fixture")
                .base_url(&url)
                .credential(Some("fixture-key"))
                .auth_style(AuthStyle::Bearer)
                .build(),
        )
    };
    let original_input =
        "token=fixture-token api_key=fixture-key https://example.test/?q=rust#page";
    let messages = [ChatMessage::user(original_input)];
    let response = scope_provider_capture(
        h.admission.clone(),
        ProviderDispatch::from_ref(&*provider).chat(
            ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            "fixture",
            None,
        ),
    )
    .await
    .expect("provider response");
    assert_eq!(response.text.as_deref(), Some("answer"));
    assert_eq!(response.reasoning_content.as_deref(), Some(reasoning));
    let events = h.events();
    let request = events
        .iter()
        .find_map(|event| match &event.body.data {
            EventData::ProviderRequestPrepared { request_ref, .. } => {
                Some(h.payload::<Value>(request_ref))
            }
            _ => None,
        })
        .expect("captured request");
    let messages_ref: StoredPayloadRef =
        serde_json::from_value(request["messages_ref"].clone()).expect("messages reference");
    let recorded: Vec<ChatMessage> = h.payload(&messages_ref);
    assert_eq!(recorded[0].content, original_input);
    let completed = events.last().expect("terminal outcome");
    let EventData::ProviderAttemptCompleted { response_ref, .. } = &completed.body.data else {
        panic!("completed attempt");
    };
    let stored: ProviderResponsePayload = h.payload(response_ref);
    let mut reasoning_count = 0;
    let mut unknown_refs = Vec::new();
    for block in &stored.blocks {
        match block {
            ResponseBlock::Reasoning { payload_ref, .. } => {
                reasoning_count += 1;
                assert_eq!(
                    payload_bytes(&h, payload_ref),
                    reasoning.to_owned().as_bytes(),
                    "{backend} {adapter} {field}",
                );
            }
            ResponseBlock::Other {
                type_name,
                classified_body_ref,
            } => {
                let bytes = payload_bytes(&h, classified_body_ref);
                let value: Value = serde_json::from_slice(&bytes).expect("unknown content");
                assert_eq!(value["token"], "fixture-token");
                match type_name.as_str() {
                    "fixture.future" => assert_eq!(value["api_key"], "fixture-key"),
                    "gemini.part_replay" => {
                        assert_eq!(value["thoughtSignature"], "sk-fixture-spoof");
                    }
                    "assistant.future_extension" => {
                        assert_eq!(value["api_key"], json!({"nested":"fixture-key"}));
                        assert_eq!(value["payload_id"], "wire-value");
                    }
                    other => panic!("unexpected unknown block: {other}"),
                }
                assert_eq!(classified_body_ref.codec, "session.reasoning/v1");
                unknown_refs.push(classified_body_ref);
            }
            ResponseBlock::Text { text_ref } => {
                assert_eq!(payload_bytes(&h, text_ref), b"answer");
            }
            other => panic!("unexpected content: {other:?}"),
        }
    }
    assert_eq!(reasoning_count, 1, "no duplicate compatibility reasoning");
    assert_eq!(unknown_refs.len(), 3);
    let response_index = completed
        .body
        .data
        .stored_refs()
        .expect("event references")
        .iter()
        .position(|reference| reference == response_ref)
        .expect("response reference");
    let references = response_references_for_read(&canonical_json(&stored).expect("response"))
        .expect("read references");
    let owner = Arc::new(SessionCoordinator::new(h.store.clone()));
    let query = owner.query(PrincipalId::shared_operator(), Arc::new(ReadAllow));
    for unknown in unknown_refs {
        let index = references
            .references
            .iter()
            .position(|reference| match reference {
                ResponseReadReference::Content(reference)
                | ResponseReadReference::Reasoning(reference) => reference == unknown,
                ResponseReadReference::Unknown => false,
            })
            .expect("unknown content reference");
        let read = query
            .read_payload(
                h.stream.clone(),
                PayloadSelector {
                    event_sequence: completed.sequence,
                    reference_path: vec![response_index, index],
                },
                1024 * 1024,
            )
            .await;
        assert!(
            matches!(read, Err(SessionError::PermissionDenied(_))),
            "classification must preserve the opaque disclosure boundary",
        );
    }
    server.abort();
    let _ = server.await;
}
