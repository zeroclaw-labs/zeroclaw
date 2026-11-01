use super::*;
use futures_util::StreamExt;
use zeroclaw_providers::compatible::{AuthStyle, OpenAiCompatibleModelProvider};

fn frame(value: serde_json::Value) -> String {
    format!("data: {value}\n\n")
}

fn payload_bytes(store: &dyn SessionEventStore, reference: &StoredPayloadRef) -> Vec<u8> {
    let PayloadRead::Present(payload) = store.read_payload(&reference.payload_id).unwrap() else {
        panic!("fixture payload must remain available");
    };
    payload.body
}

fn observed_blocks(h: &Harness, events: &[SessionEvent]) -> Vec<ResponseBlock> {
    let mut blocks = Vec::new();
    for event in events {
        if let EventData::ProviderResponseProgressObserved { observation_ref } = &event.body.data {
            let observation: ProviderObservationPayload = h.payload(observation_ref);
            for operation in observation.operations {
                let reference = match operation {
                    ProviderObservation::ProtocolSegmentObserved { segment_ref, .. } => segment_ref,
                    ProviderObservation::BlockFinalized {
                        finalized_block_ref,
                        ..
                    } => finalized_block_ref,
                    _ => continue,
                };
                blocks.push(h.payload(&reference));
            }
        }
    }
    blocks
}

fn assert_preserved_content(store: &dyn SessionEventStore, blocks: &[ResponseBlock]) {
    let mut text = Vec::new();
    let mut audio = Vec::new();
    let mut media = 0;
    let mut reasoning = 0;
    let mut unknown = 0;
    let mut extension = 0;
    let mut audio_metadata = Vec::new();
    let mut invalid_audio = 0;
    for block in semantic_blocks(store, blocks) {
        match block {
            ResponseBlock::Text { text_ref } => {
                text.push(String::from_utf8(payload_bytes(store, text_ref)).unwrap());
            }
            ResponseBlock::Reasoning { payload_ref, .. } => {
                assert_eq!(payload_bytes(store, payload_ref), b"stream thinking");
                reasoning += 1;
            }
            ResponseBlock::Media {
                media_descriptor_ref,
                received: MediaReceived::Captured { representation_ref },
            } => {
                let descriptor: serde_json::Value =
                    serde_json::from_slice(&payload_bytes(store, media_descriptor_ref)).unwrap();
                let bytes = payload_bytes(store, representation_ref);
                if descriptor["media_type"] == "audio" {
                    audio.push(bytes);
                } else {
                    assert_eq!(descriptor["media_type"], "image");
                    assert_eq!(bytes, vec![1, 2, 3]);
                }
                media += 1;
            }
            ResponseBlock::Other {
                type_name,
                classified_body_ref,
            } => {
                let value: serde_json::Value =
                    serde_json::from_slice(&payload_bytes(store, classified_body_ref)).unwrap();
                match type_name.as_str() {
                    "fixture.future_part" => {
                        assert_eq!(value["opaque"], "retained part");
                        assert_eq!(value["token"], "synthetic-part-token");
                        unknown += 1;
                    }
                    "assistant.future_extension" => {
                        assert_eq!(value["payload_id"], "wire-data-not-a-reference");
                        assert_eq!(value["token"], "synthetic-extension-token");
                        extension += 1;
                    }
                    "chat_completions.audio_delta" => audio_metadata.push(value),
                    "audio" => {
                        assert_eq!(value["source"]["data"], "Y");
                        invalid_audio += 1;
                    }
                    other => panic!("unexpected preserved block: {other}: {value}"),
                }
            }
            other => panic!("unexpected block: {other:?}"),
        }
    }
    assert_eq!(text, vec!["A", "B", "checkpoint"]);
    assert_eq!(audio, vec![vec![4, 5, 6], vec![7, 8, 9]]);
    assert_eq!(
        (media, reasoning, unknown, extension, invalid_audio),
        (3, 1, 1, 1, 1)
    );
    assert_eq!(audio_metadata.len(), 3);
    assert_eq!(audio_metadata[0]["metadata"]["id"], "audio-fixture");
    assert_eq!(audio_metadata[0]["metadata"]["transcript"], "first spoken");
    assert_eq!(audio_metadata[0]["metadata"]["expires_at"], 42);
    assert_eq!(audio_metadata[1]["metadata"]["transcript"], "second spoken");
    assert!(
        audio_metadata
            .iter()
            .all(|value| value["kind"] == "partial_content")
    );
}

#[tokio::test]
async fn compatible_multimodal_streams_capture_before_completion_eof_and_error() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    for backend in ["sqlite", "jsonl"] {
        for api in ["structured", "system", "history"] {
            for owned in [false, true] {
                for terminal in ["complete", "eof", "error"] {
                    let h = Harness::new(backend);
                    let release = Arc::new(tokio::sync::Notify::new());
                    let calls = Arc::new(AtomicUsize::new(0));
                    let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
                    let prefix = frame(serde_json::json!({"choices":[{"index":7,"delta":{
                        "reasoning_content":"stream thinking",
                        "content":[
                            {"index":3,"type":"text","text":"A"},
                            {"index":4,"type":"image","source":{"data":"AQID","media_type":"image/png"}},
                            {"index":5,"type":"fixture.future_part","opaque":"retained part","token":"synthetic-part-token"},
                            {"index":6,"type":"text","text":"B"}
                        ],
                        "audio":{"data":"BAUG","format":"wav","id":"audio-fixture","transcript":"first spoken","expires_at":42},
                        "future_extension":{"payload_id":"wire-data-not-a-reference","token":"synthetic-extension-token"}
                    }}]}))
                        + &frame(serde_json::json!({"choices":[{"index":7,"delta":{
                            "audio":{"data":"BwgJ","format":"wav","id":"audio-fixture","transcript":"second spoken"}
                        }}]}))
                        + &frame(serde_json::json!({"choices":[{"index":7,"delta":{
                            "audio":{"data":"Y","id":"audio-fixture"}
                        }}]}))
                        + &frame(serde_json::json!({"choices":[{"index":7,"delta":{
                            "content":[{"index":99,"type":"text","text":"checkpoint"}]
                        }}]}));
                    let suffix = match terminal {
                        "complete" => {
                            frame(
                                serde_json::json!({"choices":[{"index":7,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":4}}),
                            ) + "data: [DONE]\n\n"
                        }
                        "error" => frame(
                            serde_json::json!({"error":{"code":"fixture_stream_failure","message":"fixture protocol failure"}}),
                        ),
                        _ => String::new(),
                    };
                    let app =
                        axum::Router::new().route(
                            "/chat/completions",
                            axum::routing::post({
                                let release = release.clone();
                                let calls = calls.clone();
                                let bodies = bodies.clone();
                                move |axum::Json(body): axum::Json<serde_json::Value>| {
                                    let release = release.clone();
                                    let prefix = prefix.clone();
                                    let suffix = suffix.clone();
                                    calls.fetch_add(1, Ordering::SeqCst);
                                    bodies.lock().unwrap().push(body);
                                    async move {
                                        let body = futures_util::stream::once(async move {
                                            Ok::<_, std::convert::Infallible>(prefix)
                                        })
                                        .chain(futures_util::stream::once(async move {
                                            release.notified().await;
                                            Ok::<_, std::convert::Infallible>(suffix)
                                        }));
                                        axum::response::Response::builder()
                                            .header("content-type", "text/event-stream")
                                            .body(axum::body::Body::from_stream(body))
                                            .unwrap()
                                    }
                                }
                            }),
                        );
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let address = listener.local_addr().unwrap();
                    let server = zeroclaw_spawn::spawn!(async move {
                        axum::serve(listener, app).await.unwrap();
                    });
                    let provider: Arc<dyn ModelProvider> = Arc::new(
                        OpenAiCompatibleModelProvider::builder("stream-fixture")
                            .display_name("stream-fixture")
                            .base_url(&format!("http://{address}"))
                            .auth_style(AuthStyle::Bearer)
                            .build(),
                    );
                    scope_provider_capture(h.admission.clone(), async {
                        let messages = [
                            ChatMessage::system("system fixture"),
                            ChatMessage::user("previous"),
                            ChatMessage {
                                tool_calls: Vec::new(),
                                tool_result: None,
                                reasoning_content: None,
                                role: "assistant".into(),
                                content: "previous answer".into(),

                                attachments: Vec::new(),
                            },
                            ChatMessage::user("current"),
                        ];
                        let options = StreamOptions {
                            enabled: true,
                            count_tokens: true,
                        };
                        macro_rules! make_stream {
                            ($dispatch:expr) => {
                                match api {
                                    "system" => $dispatch
                                        .stream_chat_with_system(
                                            Some("system fixture"),
                                            "current",
                                            "fixture",
                                            None,
                                            options,
                                        )
                                        .map(|chunk| chunk.map(StreamEvent::from_chunk))
                                        .boxed(),
                                    "history" => $dispatch
                                        .stream_chat_with_history(
                                            &messages, "fixture", None, options,
                                        )
                                        .map(|chunk| chunk.map(StreamEvent::from_chunk))
                                        .boxed(),
                                    _ => $dispatch.stream_chat(
                                        ChatRequest {
                                            messages: &messages,
                                            tools: None,
                                            thinking: None,
                                        },
                                        "fixture",
                                        None,
                                        options,
                                    ),
                                }
                            };
                        }
                        let mut stream = if owned {
                            make_stream!(ProviderDispatch::new(provider.clone()))
                        } else {
                            make_stream!(ProviderDispatch::from_ref(&*provider))
                        };
                        let mut displayed = String::new();
                        let mut reasoning = String::new();
                        let mut tokens = 0;
                        tokio::time::timeout(Duration::from_secs(5), async {
                            while let Some(event) = stream.next().await {
                                if let StreamEvent::TextDelta(chunk) =
                                    event.expect("prefix must decode without waiting for terminal")
                                {
                                    displayed.push_str(&chunk.delta);
                                    if let Some(text) = chunk.reasoning {
                                        reasoning.push_str(&text);
                                    }
                                    tokens += chunk.token_count;
                                    if chunk.delta == "checkpoint" {
                                        return;
                                    }
                                }
                            }
                            panic!("stream ended before its captured checkpoint");
                        })
                        .await
                        .expect("content must become durable before the terminal frame");
                        assert_eq!(displayed, "ABcheckpoint");
                        assert_eq!(reasoning, "stream thinking");
                        assert!(tokens > 0);
                        let before = h.events();
                        assert!(!before.iter().any(|event| matches!(
                            event.body.data,
                            EventData::ProviderAttemptCompleted { .. }
                                | EventData::ProviderAttemptFailed { .. }
                                | EventData::InvocationOutcomeUnknown { .. }
                        )));
                        assert_preserved_content(&*h.store, &observed_blocks(&h, &before));
                        let positions: Vec<_> = before
                            .iter()
                            .filter_map(|event| match &event.body.data {
                                EventData::ProviderResponseProgressObserved { observation_ref } => {
                                    Some(h.payload::<ProviderObservationPayload>(observation_ref))
                                }
                                _ => None,
                            })
                            .flat_map(|observation| observation.operations)
                            .filter_map(|operation| match operation {
                                ProviderObservation::ProtocolSegmentObserved {
                                    position, ..
                                } => Some(position),
                                _ => None,
                            })
                            .collect();
                        assert!(positions.iter().all(|position| position.output_index == 7));
                        assert!(
                            positions
                                .iter()
                                .any(|position| position.content_index == Some(5))
                        );
                        release.notify_one();
                        let rest = tokio::time::timeout(
                            Duration::from_secs(5),
                            stream.collect::<Vec<_>>(),
                        )
                        .await
                        .unwrap();
                        let finals = rest
                            .iter()
                            .filter(|event| matches!(event, Ok(StreamEvent::Final)))
                            .count();
                        let errors = rest.iter().filter(|event| event.is_err()).count();
                        assert_eq!(
                            (finals, errors),
                            if terminal == "complete" {
                                (1, 0)
                            } else {
                                (0, 1)
                            }
                        );
                    })
                    .await;
                    let events = h.events();
                    let terminals: Vec<_> = events
                        .iter()
                        .filter(|event| {
                            matches!(
                                event.body.data,
                                EventData::ProviderAttemptCompleted { .. }
                                    | EventData::ProviderAttemptFailed { .. }
                                    | EventData::InvocationOutcomeUnknown { .. }
                            )
                        })
                        .collect();
                    assert_eq!(terminals.len(), 1);
                    let reference = match &terminals[0].body.data {
                        EventData::ProviderAttemptCompleted { response_ref, .. }
                            if terminal == "complete" =>
                        {
                            response_ref
                        }
                        EventData::ProviderAttemptFailed {
                            partial_response_ref: Some(reference),
                            ..
                        } if terminal == "error" => reference,
                        EventData::InvocationOutcomeUnknown {
                            observed_partial_ref: Some(reference),
                            ..
                        } if terminal == "eof" => reference,
                        other => panic!("unexpected terminal for {terminal}: {other:?}"),
                    };
                    let response: ProviderResponsePayload = h.payload(reference);
                    assert_eq!(
                        response.completion,
                        if terminal == "complete" {
                            ContentCompletion::Complete
                        } else {
                            ContentCompletion::Partial
                        }
                    );
                    assert_preserved_content(&*h.store, &response.blocks);
                    assert_eq!(response.usage_ref.is_some(), terminal == "complete");
                    {
                        let bodies = bodies.lock().unwrap();
                        let roles: Vec<_> = bodies[0]["messages"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|message| message["role"].as_str().unwrap())
                            .collect();
                        assert_eq!(
                            roles,
                            if api == "system" {
                                vec!["system", "user"]
                            } else {
                                vec!["system", "user", "assistant", "user"]
                            }
                        );
                    }
                    let reopened = open_event_store(h._dir.path(), backend).unwrap();
                    assert_eq!(
                        reopened.head(&h.stream).unwrap(),
                        h.store.head(&h.stream).unwrap()
                    );
                    assert_eq!(
                        serde_json::from_slice::<ProviderResponsePayload>(&payload_bytes(
                            &*reopened, reference
                        ))
                        .unwrap(),
                        response
                    );
                    assert_preserved_content(&*reopened, &response.blocks);
                    assert_eq!(calls.load(Ordering::SeqCst), 1);
                    server.abort();
                    assert!(server.await.unwrap_err().is_cancelled());
                }
            }
        }
    }
}

#[tokio::test]
async fn compatible_stream_decode_failures_preserve_valid_prefix_and_signed_reasoning() {
    for backend in ["sqlite", "jsonl"] {
        for api in ["structured", "system", "history"] {
            for ending in [
                "invalid_utf8",
                "invalid_after_finish",
                "invalid_after_done",
                "incomplete_utf8",
                "unfinished_line",
                "unfinished_frame",
                "split_utf8",
            ] {
                let h = Harness::new(backend);
                let thinking = serde_json::json!({
                    "index":11, "type":"thinking", "thinking":"api_key=fixture-signed-thought",
                    "signature":"sk-fixtureSignedContinuation"
                });
                let redacted = serde_json::json!({
                    "index":12, "type":"redacted_thinking", "data":"sk-fixtureOpaqueContinuation"
                });
                let prefix = frame(serde_json::json!({"choices":[{"index":7,"delta":{
                    "reasoning_content":"api_key=fixture-readable-thought",
                    "content":[
                        {"index":1,"type":"text","text":"valid prefix"},
                        {"index":2,"type":"image","source":{"data":"AQID","media_type":"image/png"}}
                    ],
                    "thinking_blocks":[thinking.clone(),redacted.clone()]
                }}]}));
                let finish = frame(serde_json::json!({
                    "choices":[{"index":7,"delta":{},"finish_reason":"stop"}]
                }));
                let mut bytes = prefix.into_bytes();
                let chunks = match ending {
                    "invalid_utf8" => {
                        bytes.push(0xff);
                        vec![bytes]
                    }
                    "invalid_after_finish" | "invalid_after_done" => {
                        bytes.extend_from_slice(finish.as_bytes());
                        if ending == "invalid_after_done" {
                            bytes.extend_from_slice(b"data: [DONE]\n\n");
                        }
                        bytes.push(0xff);
                        vec![bytes]
                    }
                    "incomplete_utf8" => {
                        bytes.extend_from_slice(&[0xe4, 0xb8]);
                        vec![bytes]
                    }
                    "unfinished_line" => {
                        bytes.extend_from_slice(b"data: {");
                        vec![bytes]
                    }
                    "unfinished_frame" => {
                        bytes.extend_from_slice(finish.trim_end_matches('\n').as_bytes());
                        bytes.push(b'\n');
                        vec![bytes]
                    }
                    "split_utf8" => {
                        let content = frame(serde_json::json!({"choices":[{"index":7,"delta":{
                            "content":"\u{4e2d}\u{6587}"
                        }}]}));
                        let split = content.find('\u{4e2d}').unwrap() + 1;
                        bytes.extend_from_slice(&content.as_bytes()[..split]);
                        let mut rest = content.as_bytes()[split..].to_vec();
                        rest.extend_from_slice(finish.as_bytes());
                        rest.extend_from_slice(b"data: [DONE]\n\n");
                        vec![bytes, rest]
                    }
                    _ => unreachable!(),
                };
                let app = axum::Router::new().route(
                    "/chat/completions",
                    axum::routing::post(move || {
                        let chunks = chunks.clone();
                        async move {
                            axum::response::Response::builder()
                                .header("content-type", "text/event-stream")
                                .body(axum::body::Body::from_stream(futures_util::stream::iter(
                                    chunks.into_iter().map(Ok::<_, std::convert::Infallible>),
                                )))
                                .unwrap()
                        }
                    }),
                );
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let server = zeroclaw_spawn::spawn!(async move {
                    axum::serve(listener, app).await.unwrap();
                });
                let provider: Arc<dyn ModelProvider> = Arc::new(
                    OpenAiCompatibleModelProvider::builder("decode-fixture")
                        .display_name("decode-fixture")
                        .base_url(&format!("http://{address}"))
                        .auth_style(AuthStyle::Bearer)
                        .build(),
                );
                let output = scope_provider_capture(h.admission.clone(), async {
                    let dispatch = ProviderDispatch::new(provider);
                    let messages = [ChatMessage::user("fixture")];
                    let options = StreamOptions::new(true);
                    let stream = match api {
                        "system" => dispatch
                            .stream_chat_with_system(None, "fixture", "fixture", None, options)
                            .map(|chunk| chunk.map(StreamEvent::from_chunk))
                            .boxed(),
                        "history" => dispatch
                            .stream_chat_with_history(&messages, "fixture", None, options)
                            .map(|chunk| chunk.map(StreamEvent::from_chunk))
                            .boxed(),
                        _ => dispatch.stream_chat(
                            ChatRequest {
                                messages: &messages,
                                tools: None,
                                thinking: None,
                            },
                            "fixture",
                            None,
                            options,
                        ),
                    };
                    tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        stream.collect::<Vec<_>>(),
                    )
                    .await
                    .unwrap()
                })
                .await;
                let success = ending == "split_utf8";
                assert_eq!(
                    output.iter().filter(|event| event.is_err()).count(),
                    usize::from(!success),
                    "{backend} {api} {ending}"
                );
                assert_eq!(
                    output
                        .iter()
                        .filter(|event| matches!(event, Ok(StreamEvent::Final)))
                        .count(),
                    usize::from(success)
                );
                if !success {
                    let error = output
                        .iter()
                        .find_map(|event| event.as_ref().err())
                        .expect("decode error");
                    let causes = std::iter::successors(
                        Some(error as &(dyn std::error::Error + 'static)),
                        |cause| cause.source(),
                    );
                    assert!(
                        causes.into_iter().any(|cause| matches!(
                            cause.downcast_ref::<StreamError>(),
                            Some(StreamError::InvalidSse(_))
                        )),
                        "{backend} {api} {ending}: {error:?}"
                    );
                }
                let events = h.events();
                let terminals: Vec<_> = events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event.body.data,
                            EventData::ProviderAttemptCompleted { .. }
                                | EventData::ProviderAttemptFailed { .. }
                                | EventData::InvocationOutcomeUnknown { .. }
                        )
                    })
                    .collect();
                assert_eq!(terminals.len(), 1);
                let (response_ref, error_ref) = match &terminals[0].body.data {
                    EventData::ProviderAttemptCompleted { response_ref, .. } if success => {
                        (response_ref, None)
                    }
                    EventData::InvocationOutcomeUnknown {
                        observed_partial_ref: Some(response_ref),
                        error_ref: Some(error_ref),
                        ..
                    } if !success => (response_ref, Some(error_ref)),
                    other => panic!("unexpected terminal for {ending}: {other:?}"),
                };
                let response: ProviderResponsePayload = h.payload(response_ref);
                assert_eq!(
                    response.completion,
                    if success {
                        ContentCompletion::Complete
                    } else {
                        ContentCompletion::Partial
                    }
                );
                let mut texts = String::new();
                let mut reasoning = Vec::new();
                let mut media = 0;
                for block in semantic_blocks(h.store.as_ref(), &response.blocks) {
                    match block {
                        ResponseBlock::Text { text_ref } => texts.push_str(
                            &String::from_utf8(payload_bytes(&*h.store, text_ref)).unwrap(),
                        ),
                        ResponseBlock::Reasoning { payload_ref, .. } => {
                            reasoning.push(payload_bytes(&*h.store, payload_ref))
                        }
                        ResponseBlock::Media {
                            received: MediaReceived::Captured { representation_ref },
                            ..
                        } => {
                            assert_eq!(payload_bytes(&*h.store, representation_ref), vec![1, 2, 3]);
                            media += 1;
                        }
                        other => panic!("unexpected preserved prefix block: {other:?}"),
                    }
                }
                assert_eq!(
                    texts,
                    if success {
                        "valid prefix\u{4e2d}\u{6587}"
                    } else {
                        "valid prefix"
                    }
                );
                assert_eq!(media, 1);
                assert_eq!(reasoning.len(), 3);
                assert!(reasoning.contains(&b"api_key=fixture-readable-thought".to_vec()));
                assert!(reasoning.contains(&thinking.to_string().into_bytes()));
                assert!(reasoning.contains(&redacted.to_string().into_bytes()));
                if let Some(error_ref) = error_ref {
                    let diagnostic: ProviderDiagnostic = h.payload(error_ref);
                    assert_eq!(diagnostic.error_key, "provider_decode_error");
                    assert_eq!(diagnostic.stage, DiagnosticStage::Decode);
                    let DiagnosticDetail::Available { detail_ref } = diagnostic.detail else {
                        panic!("decode diagnostic must retain its detail");
                    };
                    let detail = String::from_utf8(payload_bytes(&*h.store, &detail_ref)).unwrap();
                    assert!(detail.contains(
                        if ending.starts_with("invalid") || ending == "incomplete_utf8" {
                            "UTF-8"
                        } else {
                            "Unterminated SSE frame"
                        }
                    ));
                }
                let reopened = open_event_store(h._dir.path(), backend).unwrap();
                assert_eq!(
                    reopened.head(&h.stream).unwrap(),
                    h.store.head(&h.stream).unwrap()
                );
                let restored: ProviderResponsePayload =
                    serde_json::from_slice(&payload_bytes(&*reopened, response_ref)).unwrap();
                assert_eq!(restored, response);
                for block in &restored.blocks {
                    let (_, reference) = block.component();
                    assert!(!payload_bytes(&*reopened, reference).is_empty());
                }
                server.abort();
                assert!(server.await.unwrap_err().is_cancelled());
            }
        }
    }
}
